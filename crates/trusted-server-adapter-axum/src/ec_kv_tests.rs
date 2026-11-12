//! Tests through the real SDK HTTP protocol, without ambient AWS credentials.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use aws_config::BehaviorVersion;
use aws_sdk_dynamodb::Client;
use aws_sdk_dynamodb::config::{Credentials, Region};
use aws_sdk_dynamodb::types::{
    AttributeDefinition, AttributeValue, BillingMode, KeySchemaElement, KeyType,
    ScalarAttributeType,
};
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, Request, StatusCode};
use axum::response::IntoResponse;
use edgezero_core::body::Body;
use edgezero_core::http::Response;
use futures::FutureExt as _;
use serde_json::{Value, json};
use trusted_server_core::ec::EcContext;
use trusted_server_core::ec::finalize::ec_finalize_response;
use trusted_server_core::ec::kv::{KvIdentityGraph, UpsertResult};
use trusted_server_core::ec::kv_backend::{EcKvStore, EcKvWrite, EcKvWriteMode, EcKvWriteOutcome};
use trusted_server_core::ec::kv_types::KvEntry;
use trusted_server_core::ec::registry::PartnerRegistry;
use trusted_server_core::platform::GeoInfo;
use trusted_server_core::test_support::nextjs_auction::{self, NextJsAuctionOrigin};

use super::DynamoDbEcKv;

const TABLE: &str = "ec-fixture";
const NS: &str = "fixture";

type Requests = Arc<Mutex<Vec<(String, Value)>>>;
type Responses = Arc<Mutex<VecDeque<(StatusCode, Value)>>>;

struct Fixture {
    provider: Arc<DynamoDbEcKv>,
    requests: Requests,
    responses: Responses,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn client(endpoint: &str) -> Client {
    Client::from_conf(
        aws_sdk_dynamodb::Config::builder()
            .behavior_version(BehaviorVersion::v2026_01_12())
            .region(Region::new("us-east-1"))
            .credentials_provider(Credentials::new(
                "fixture", "fixture", None, None, "fixture",
            ))
            .endpoint_url(endpoint)
            .build(),
    )
}

fn table_description() -> Value {
    json!({"Table": {
        "TableName": TABLE, "TableStatus": "ACTIVE",
        "KeySchema": [{"AttributeName":"pk", "KeyType":"HASH"}, {"AttributeName":"sk", "KeyType":"RANGE"}],
        "AttributeDefinitions": [{"AttributeName":"pk", "AttributeType":"S"}, {"AttributeName":"sk", "AttributeType":"S"}]
    }})
}

#[tokio::test(flavor = "current_thread")]
async fn typed_local_preparation_uses_envelope_binding_and_dummy_credentials() {
    let captured = Arc::new(Mutex::new(Vec::<HeaderMap>::new()));
    let headers = Arc::clone(&captured);
    let router = axum::Router::new().fallback(move |request: Request<axum::body::Body>| {
        headers
            .lock()
            .expect("should capture typed preparation request")
            .push(request.headers().clone());
        async { axum::Json(table_description()) }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("should bind typed preparation fixture");
    let endpoint = format!(
        "http://{}",
        listener
            .local_addr()
            .expect("should read local fixture address")
    );
    let task = tokio::spawn(async move {
        axum::serve(listener, router)
            .await
            .expect("should serve typed preparation fixture");
    });
    let config = trusted_server_core::ec::storage::DynamoDbEcStorage {
        region: "us-east-1".to_owned(),
        table: TABLE.to_owned(),
        namespace: " exact ".to_owned(),
        local_endpoint: Some(endpoint),
    };
    let provider = super::prepare(&config)
        .await
        .expect("should prepare from typed envelope fields");
    assert_eq!(
        provider.partition, "ec: exact ",
        "should preserve exact namespace bytes"
    );
    let headers = captured.lock().expect("should inspect preparation headers");
    assert_eq!(headers.len(), 1, "should not retry preparation");
    let authorization = headers[0]
        .get("authorization")
        .expect("should sign local request")
        .to_str()
        .expect("should decode local authorization");
    assert!(
        authorization.contains("Credential=local/"),
        "should bypass ambient credentials with explicit local dummy credentials"
    );
    task.abort();
}

async fn fixture() -> Fixture {
    let requests: Requests = Arc::new(Mutex::new(Vec::new()));
    let responses: Responses = Arc::new(Mutex::new(VecDeque::from([(
        StatusCode::OK,
        table_description(),
    )])));
    let router = axum::Router::new()
        .fallback(
            async |State((requests, responses)): State<(Requests, Responses)>,
                   headers: HeaderMap,
                   body: Bytes| {
                let target = headers
                    .get("x-amz-target")
                    .expect("should receive SDK operation target")
                    .to_str()
                    .expect("should decode SDK operation target")
                    .to_owned();
                requests
                    .lock()
                    .expect("should lock recorded SDK requests")
                    .push((
                        target,
                        serde_json::from_slice(&body).expect("should decode SDK JSON request"),
                    ));
                let (status, body) = responses
                    .lock()
                    .expect("should lock queued SDK responses")
                    .pop_front()
                    .expect("unexpected SDK request");
                (
                    status,
                    [("content-type", "application/x-amz-json-1.0")],
                    body.to_string(),
                )
                    .into_response()
            },
        )
        .with_state((Arc::clone(&requests), Arc::clone(&responses)));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("should bind loopback wire fixture");
    let endpoint = format!(
        "http://{}",
        listener
            .local_addr()
            .expect("should read wire fixture address")
    );
    let task = tokio::spawn(async move {
        axum::serve(listener, router)
            .await
            .expect("should serve SDK wire fixture");
    });
    let provider = DynamoDbEcKv::prepare_with_client(client(&endpoint), TABLE, NS)
        .await
        .expect("should prepare SDK wire provider");
    Fixture {
        provider,
        requests,
        responses,
        task,
    }
}

impl Fixture {
    fn queue(&self, value: Value) {
        self.responses
            .lock()
            .expect("should lock queued wire response")
            .push_back((StatusCode::OK, value));
    }
    fn conflict(&self) {
        self.responses.lock().expect("should queue conditional conflict response").push_back((StatusCode::BAD_REQUEST, json!({"__type":"com.amazonaws.dynamodb.v20120810#ConditionalCheckFailedException", "message":"private key and body"})));
    }
    fn fail(&self) {
        self.responses
            .lock()
            .expect("should queue denied response")
            .push_back((
                StatusCode::FORBIDDEN,
                json!({"__type":"AccessDeniedException", "message":"private credential and row"}),
            ));
    }
    fn allocate(&self, generation: u64) {
        self.queue(json!({"Attributes":{"generation":{"N":generation.to_string()}}}));
    }
    fn requests(&self) -> Vec<(String, Value)> {
        self.requests
            .lock()
            .expect("should lock recorded requests")
            .clone()
    }
}

fn row(key: &str, expires: u64) -> Value {
    json!({"pk":{"S":"ec:fixture"}, "sk":{"S":key}, "body":{"B":"Ym9keQ=="}, "metadata":{"B":"bWV0YQ=="}, "generation":{"N":"7"}, "expires_at_ms":{"N":expires.to_string()}, "ttl_epoch_s":{"N":expires.div_ceil(1_000).to_string()}})
}

fn write(mode: EcKvWriteMode) -> EcKvWrite<'static> {
    EcKvWrite {
        body: "body",
        metadata: "meta",
        ttl: Duration::from_secs(60),
        mode,
    }
}

#[tokio::test(flavor = "current_thread")]
async fn conditional_writes_use_real_sdk_expressions_and_allocator_partition() {
    let f = fixture().await;
    f.allocate(1);
    f.queue(json!({}));
    assert_eq!(
        f.provider
            .insert("key", write(EcKvWriteMode::Add))
            .await
            .expect("should perform Add wire write"),
        EcKvWriteOutcome::Written
    );
    f.allocate(2);
    f.conflict();
    assert_eq!(
        f.provider
            .insert("key", write(EcKvWriteMode::Add))
            .await
            .expect("should classify Add conflict"),
        EcKvWriteOutcome::PreconditionFailed
    );
    f.allocate(3);
    f.conflict();
    assert_eq!(
        f.provider
            .insert("key", write(EcKvWriteMode::IfGenerationMatch(1)))
            .await
            .expect("should classify stale CAS conflict"),
        EcKvWriteOutcome::PreconditionFailed
    );
    f.allocate(4);
    f.queue(json!({}));
    assert_eq!(
        f.provider
            .insert("key", write(EcKvWriteMode::Overwrite))
            .await
            .expect("should perform overwrite wire write"),
        EcKvWriteOutcome::Written
    );
    let requests = f.requests();
    for (_, req) in requests
        .iter()
        .filter(|(target, _)| target.ends_with("UpdateItem"))
    {
        assert_eq!(
            req["Key"],
            json!({"pk":{"S":"ec-allocator:fixture"}, "sk":{"S":"generation"}})
        );
        assert_eq!(req["UpdateExpression"], "ADD #g :one");
        assert_eq!(
            req["ConditionExpression"],
            "attribute_not_exists(#g) OR #g < :max"
        );
        assert_eq!(
            req["ExpressionAttributeValues"][":max"]["N"],
            u64::MAX.to_string()
        );
        assert!(req.get("ttl_epoch_s").is_none());
    }
    let puts: Vec<_> = requests
        .iter()
        .filter(|(target, _)| target.ends_with("PutItem"))
        .map(|(_, r)| r)
        .collect();
    assert_eq!(
        puts[0]["ConditionExpression"],
        "attribute_not_exists(#pk) OR #e <= :now"
    );
    assert_eq!(
        puts[2]["ConditionExpression"],
        "#g = :expected AND #e > :now"
    );
    assert_eq!(puts[2]["ExpressionAttributeValues"][":expected"]["N"], "1");
    assert!(puts[3].get("ConditionExpression").is_none());
    for (i, put) in puts.into_iter().enumerate() {
        assert_eq!(put["Item"]["generation"]["N"], (i + 1).to_string());
        assert_eq!(put["Item"]["body"]["B"], "Ym9keQ==");
        assert_eq!(put["Item"]["metadata"]["B"], "bWV0YQ==");
    }
}

#[tokio::test(flavor = "current_thread")]
async fn lookup_expires_logically_but_exact_physical_existence_can_fence_withdrawal() {
    let f = fixture().await;
    f.queue(json!({"Item":row("key", u64::MAX)}));
    let found = f
        .provider
        .lookup("key")
        .await
        .expect("should read SDK wire row")
        .expect("should retain SDK wire row");
    assert_eq!(found.body, b"body");
    assert_eq!(
        found.metadata.expect("should retain wire metadata"),
        b"meta"
    );
    assert_eq!(found.generation, 7);
    f.queue(json!({"Item":row("key", 1)}));
    assert!(
        f.provider
            .lookup("key")
            .await
            .expect("should hide logically expired body")
            .is_none()
    );
    f.queue(json!({"Item":row("key", 1)}));
    assert!(
        f.provider
            .key_exists("key")
            .await
            .expect("should confirm physical expired row for withdrawal fencing")
    );
    f.queue(json!({}));
    assert!(
        !f.provider
            .key_exists("absent")
            .await
            .expect("should check exact absent key")
    );
    for (_, req) in f
        .requests()
        .iter()
        .filter(|(target, _)| target.ends_with("GetItem"))
    {
        assert_eq!(req["ConsistentRead"], true);
    }
}

#[tokio::test(flavor = "current_thread")]
async fn prefix_listing_drives_empty_expired_and_short_pages_to_completion() {
    let f = fixture().await;
    let cursor = |key: &str| json!({"pk":{"S":"ec:fixture"}, "sk":{"S":key}});
    f.queue(json!({"Items":[], "LastEvaluatedKey":cursor("marker:0")}));
    f.queue(json!({"Items":[row("marker:1",1)], "LastEvaluatedKey":cursor("marker:1")}));
    f.queue(json!({"Items":[row("marker:2",u64::MAX)], "LastEvaluatedKey":cursor("marker:2")}));
    f.queue(json!({"Items":[row("marker:3",u64::MAX)]}));
    assert_eq!(
        f.provider
            .list_keys_with_prefix("marker:", 3)
            .await
            .expect("should complete prefix pagination"),
        ["marker:2", "marker:3"]
    );
    let requests = f.requests();
    let queries: Vec<_> = requests
        .iter()
        .filter(|(target, _)| target.ends_with("Query"))
        .map(|(_, r)| r)
        .collect();
    assert_eq!(queries.len(), 4);
    for req in queries {
        assert_eq!(req["ConsistentRead"], true);
    }
}

#[tokio::test(flavor = "current_thread")]
async fn incomplete_listing_and_infrastructure_failures_never_look_like_absence_or_conflict() {
    let f = fixture().await;
    for n in 0..64 {
        f.queue(json!({"Items":[], "LastEvaluatedKey":{"pk":{"S":"ec:fixture"}, "sk":{"S":format!("p:{n:03}")}}}));
    }
    assert!(f.provider.list_keys_with_prefix("p:", 2).await.is_err());
    f.fail();
    let error = f
        .provider
        .key_exists("private-key")
        .await
        .expect_err("denied read should remain an infrastructure error");
    assert!(!format!("{error:?}").contains("private"));
    f.allocate(99);
    f.fail();
    assert!(
        f.provider
            .insert("key", write(EcKvWriteMode::Add))
            .await
            .is_err()
    );
    f.fail();
    assert!(f.provider.delete("key").await.is_err());
    assert_eq!(f.requests().len(), 69); // describe + 64 query + get + allocation + put + delete
}

#[tokio::test(flavor = "current_thread")]
async fn retryable_write_failure_is_sanitized_and_never_automatically_retried() {
    let f = fixture().await;
    f.allocate(50);
    f.responses
        .lock()
        .expect("should queue retryable failure")
        .push_back((
            StatusCode::INTERNAL_SERVER_ERROR,
            json!({"__type":"InternalServerError", "message":"private row and credentials"}),
        ));
    let error = f
        .provider
        .insert("private-key", write(EcKvWriteMode::Add))
        .await
        .expect_err("uncertain write should fail without an automatic retry");
    assert!(!format!("{error:?}").contains("private"));
    assert_eq!(
        f.requests().len(),
        3,
        "should send DescribeTable, allocator and one PutItem only"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn malformed_cursor_expiry_or_allocator_fails_closed() {
    let f = fixture().await;
    f.queue(json!({"Items":[], "LastEvaluatedKey":{"pk":{"S":"other"}, "sk":{"S":"p:1"}}}));
    assert!(f.provider.list_keys_with_prefix("p:", 2).await.is_err());
    let mut malformed = row("key", u64::MAX);
    malformed["ttl_epoch_s"] = json!({"N":"0"});
    f.queue(json!({"Item":malformed}));
    assert!(f.provider.lookup("key").await.is_err());
    f.allocate(0);
    assert!(
        f.provider
            .insert("key", write(EcKvWriteMode::Add))
            .await
            .is_err()
    );
}

/// Run explicitly against disposable `DynamoDB` Local, never AWS:
/// `TS_TEST_EC_DYNAMODB_ENDPOINT=http://127.0.0.1:8000 cargo test ... -- --ignored`
#[tokio::test(flavor = "current_thread")]
#[ignore = "requires explicitly selected disposable DynamoDB Local loopback endpoint"]
async fn dynamodb_local_contract() {
    let endpoint = std::env::var("TS_TEST_EC_DYNAMODB_ENDPOINT")
        .expect("should explicitly select a test-only Local endpoint");
    trusted_server_core::ec::storage::DynamoDbEcStorage {
        region: "us-east-1".to_owned(),
        table: "example-identity".to_owned(),
        namespace: "fixture".to_owned(),
        local_endpoint: Some(endpoint.clone()),
    }
    .validate()
    .expect("should restrict DynamoDB Local tests to a loopback root URL");
    let client = client(&endpoint);
    let table = format!(
        "ec-contract-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("should obtain table name timestamp")
            .as_nanos()
    );
    client
        .create_table()
        .table_name(&table)
        .billing_mode(BillingMode::PayPerRequest)
        .key_schema(
            KeySchemaElement::builder()
                .attribute_name("pk")
                .key_type(KeyType::Hash)
                .build()
                .expect("should build partition schema"),
        )
        .key_schema(
            KeySchemaElement::builder()
                .attribute_name("sk")
                .key_type(KeyType::Range)
                .build()
                .expect("should build sort schema"),
        )
        .attribute_definitions(
            AttributeDefinition::builder()
                .attribute_name("pk")
                .attribute_type(ScalarAttributeType::S)
                .build()
                .expect("should define partition type"),
        )
        .attribute_definitions(
            AttributeDefinition::builder()
                .attribute_name("sk")
                .attribute_type(ScalarAttributeType::S)
                .build()
                .expect("should define sort type"),
        )
        .send()
        .await
        .expect("should create disposable Local table");
    let result = std::panic::AssertUnwindSafe(async {
        let p = DynamoDbEcKv::prepare_with_client(client.clone(), &table, NS)
            .await
            .expect("should prepare Local provider");
        local_generation_contract(&client, &table, &p).await;
        local_expiry_and_listing_contract(&client, &table, &p).await;
        local_graph_withdrawal_contract(&p).await;
        local_allocator_exhaustion_contract(&client, &table, &p).await;
    })
    .catch_unwind()
    .await;
    let cleanup = client.delete_table().table_name(&table).send().await;
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
    cleanup.expect("should delete disposable Local table");
}

async fn local_generation_contract(client: &Client, table: &str, p: &DynamoDbEcKv) {
    let writes = (0..12).map(|_| p.insert("key", write(EcKvWriteMode::Add)));
    let outcomes = futures::future::join_all(writes).await;
    assert!(
        outcomes.iter().all(Result::is_ok),
        "concurrent Add must not hide infrastructure failures"
    );
    assert_eq!(
        outcomes
            .into_iter()
            .filter(|r| matches!(r, Ok(EcKvWriteOutcome::Written)))
            .count(),
        1
    );
    let initial = p
        .lookup("key")
        .await
        .expect("should read created row")
        .expect("should retain created row");
    assert_eq!(initial.body, b"body");
    assert_eq!(initial.metadata.expect("should retain metadata"), b"meta");
    assert_eq!(
        p.insert(
            "key",
            write(EcKvWriteMode::IfGenerationMatch(initial.generation))
        )
        .await
        .expect("should execute matching CAS"),
        EcKvWriteOutcome::Written
    );
    assert_eq!(
        p.insert(
            "key",
            write(EcKvWriteMode::IfGenerationMatch(initial.generation))
        )
        .await
        .expect("should execute stale CAS"),
        EcKvWriteOutcome::PreconditionFailed
    );
    let before_delete = p
        .lookup("key")
        .await
        .expect("should read before delete")
        .expect("should retain row before delete")
        .generation;
    p.delete("key").await.expect("should hard-delete row");
    let recreated = DynamoDbEcKv::prepare_with_client(client.clone(), table, NS)
        .await
        .expect("should recreate provider");
    assert_eq!(
        recreated
            .insert("key", write(EcKvWriteMode::Add))
            .await
            .expect("should recreate deleted row"),
        EcKvWriteOutcome::Written
    );
    assert!(
        recreated
            .lookup("key")
            .await
            .expect("should read recreated row")
            .expect("should retain recreated row")
            .generation
            > before_delete
    );
    assert_eq!(
        p.insert(
            "key",
            write(EcKvWriteMode::IfGenerationMatch(before_delete))
        )
        .await
        .expect("should reject pre-delete generation"),
        EcKvWriteOutcome::PreconditionFailed
    );
    assert!(!p.key_exists("ke").await.expect("should check exact key"));
    let isolated = DynamoDbEcKv::prepare_with_client(client.clone(), table, "other")
        .await
        .expect("should prepare isolated namespace");
    assert!(
        !isolated
            .key_exists("key")
            .await
            .expect("should isolate exact lookup")
    );
    assert!(
        isolated
            .list_keys_with_prefix("", 100)
            .await
            .expect("should isolate prefix listing")
            .is_empty()
    );
}

async fn local_expiry_and_listing_contract(client: &Client, table: &str, p: &DynamoDbEcKv) {
    let expiring = EcKvWrite {
        ttl: Duration::from_millis(1),
        ..write(EcKvWriteMode::Add)
    };
    p.insert("expired", expiring)
        .await
        .expect("should insert expiring row");
    tokio::time::sleep(Duration::from_millis(20)).await;
    let physical = client
        .get_item()
        .table_name(table)
        .key("pk", AttributeValue::S("ec:fixture".into()))
        .key("sk", AttributeValue::S("expired".into()))
        .consistent_read(true)
        .send()
        .await
        .expect("should read physical expired row")
        .item
        .expect("Local TTL disabled should retain physical expired row");
    let expired_generation = physical["generation"]
        .as_n()
        .expect("should store numeric generation")
        .parse::<u64>()
        .expect("should store u64 generation");
    assert!(
        p.lookup("expired")
            .await
            .expect("should check logical expiry")
            .is_none()
    );
    assert!(
        p.key_exists("expired")
            .await
            .expect("should retain physical presence for withdrawal fencing")
    );
    assert_eq!(
        p.insert(
            "expired",
            write(EcKvWriteMode::IfGenerationMatch(expired_generation))
        )
        .await
        .expect("should reject expired CAS"),
        EcKvWriteOutcome::PreconditionFailed
    );
    assert_eq!(
        p.insert("expired", write(EcKvWriteMode::Add))
            .await
            .expect("should replace logically absent row"),
        EcKvWriteOutcome::Written
    );
    p.insert(
        "__ts_ec_withdrawal_complete__:key:a",
        write(EcKvWriteMode::Add),
    )
    .await
    .expect("should support completion marker key");
    assert_eq!(
        p.count_keys_with_prefix("__ts_ec_withdrawal_complete__:", 100)
            .await
            .expect("should count completion markers"),
        1
    );
    // Query Limit=2 first evaluates only expired rows. A later short page also
    // has a continuation. The adapter must drive both before returning live keys.
    for key in ["page:000", "page:001", "page:002"] {
        p.insert(key, expiring)
            .await
            .expect("should insert expiring prefix row");
    }
    for key in ["page:003", "page:004"] {
        p.insert(key, write(EcKvWriteMode::Add))
            .await
            .expect("should insert live prefix row");
    }
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert_eq!(
        p.list_keys_with_prefix("page:", 2)
            .await
            .expect("should traverse expired Local query pages"),
        ["page:003", "page:004"]
    );
    assert_eq!(
        p.count_keys_with_prefix("page:", 3)
            .await
            .expect("should count complete live prefix"),
        2
    );
}

async fn local_allocator_exhaustion_contract(client: &Client, table: &str, p: &DynamoDbEcKv) {
    client
        .update_item()
        .table_name(table)
        .key("pk", AttributeValue::S("ec-allocator:fixture".to_owned()))
        .key("sk", AttributeValue::S("generation".to_owned()))
        .update_expression("SET #g = :near_max")
        .expression_attribute_names("#g", "generation")
        .expression_attribute_values(":near_max", AttributeValue::N((u64::MAX - 1).to_string()))
        .send()
        .await
        .expect("should seed retained allocator boundary");
    assert_eq!(
        p.insert("allocator:last", write(EcKvWriteMode::Add))
            .await
            .expect("should allocate final u64 generation"),
        EcKvWriteOutcome::Written
    );
    assert_eq!(
        p.lookup("allocator:last")
            .await
            .expect("should read final generation")
            .expect("should retain final generation")
            .generation,
        u64::MAX
    );
    assert!(
        p.insert("allocator:overflow", write(EcKvWriteMode::Add))
            .await
            .is_err()
    );
    assert!(
        !p.key_exists("allocator:overflow")
            .await
            .expect("failed allocation should not put a row")
    );
    let recreated = DynamoDbEcKv::prepare_with_client(client.clone(), table, NS)
        .await
        .expect("should recreate provider with exhausted allocator");
    assert!(
        recreated
            .insert("allocator:recreated", write(EcKvWriteMode::Add))
            .await
            .is_err()
    );
}

async fn local_graph_withdrawal_contract(p: &DynamoDbEcKv) {
    let ec = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.Ab1234";
    let settings = nextjs_auction::settings();
    let origin = NextJsAuctionOrigin::default();
    let services = nextjs_auction::services(&origin);
    let graph = KvIdentityGraph::new(p.clone());
    graph
        .create(ec, &KvEntry::minimal("initial.example", "initial-uid", 1))
        .await
        .expect("should create graph root");
    let (first, second) = futures::join!(
        graph.upsert_partner_id_if_exists(ec, "first.example", "first-uid"),
        graph.upsert_partner_id_if_exists(ec, "second.example", "second-uid"),
    );
    assert_eq!(
        first.expect("should merge first concurrent UID"),
        UpsertResult::Written
    );
    assert_eq!(
        second.expect("should merge second concurrent UID"),
        UpsertResult::Written
    );
    let (merged, _) = graph
        .get(ec)
        .await
        .expect("should read merged graph")
        .expect("should retain merged graph");
    assert_eq!(merged.ids["first.example"].uid, "first-uid");
    assert_eq!(merged.ids["second.example"].uid, "second-uid");
    let before_withdrawal = p
        .lookup(ec)
        .await
        .expect("should read pre-withdrawal row")
        .expect("should retain pre-withdrawal row");
    let stale_body = std::str::from_utf8(&before_withdrawal.body).expect("should store JSON body");
    let stale_meta = std::str::from_utf8(
        before_withdrawal
            .metadata
            .as_deref()
            .expect("should store JSON metadata"),
    )
    .expect("should encode metadata as UTF-8");
    let request = Request::builder()
        .uri("https://test-publisher.example.com/article")
        .header("cookie", format!("ts-ec={ec}"))
        .header("sec-gpc", "1")
        .body(Body::empty())
        .expect("should build synthetic withdrawal request");
    let geo = GeoInfo::country_region("US", Some("TN".to_owned()));
    let mut context =
        EcContext::read_from_request_with_geo(&settings, &request, &services, Some(&geo))
            .expect("should decode synthetic GPC withdrawal");
    let registry = PartnerRegistry::from_config(&[]).expect("should create empty fixture registry");
    let mut response = Response::new(Body::empty());
    let (_, raced) = futures::join!(
        ec_finalize_response(
            &settings,
            &mut context,
            Some(&graph),
            &registry,
            None,
            None,
            &mut response
        ),
        graph.upsert_partner_id_if_exists(ec, "late.example", "late-uid"),
    );
    assert!(matches!(
        raced.expect("should resolve partner update racing withdrawal"),
        UpsertResult::Written | UpsertResult::ConsentWithdrawn
    ));
    let (tombstone, _) = graph
        .get(ec)
        .await
        .expect("should read authoritative tombstone")
        .expect("should retain tombstone");
    assert!(!tombstone.consent.ok);
    assert!(tombstone.ids.is_empty());
    assert_eq!(
        graph
            .upsert_partner_id_if_exists(ec, "late.example", "retry-uid")
            .await
            .expect("should reject post-withdrawal UID"),
        UpsertResult::ConsentWithdrawn
    );
    assert_eq!(
        p.insert(
            ec,
            EcKvWrite {
                body: stale_body,
                metadata: stale_meta,
                ttl: Duration::from_secs(60),
                mode: EcKvWriteMode::IfGenerationMatch(before_withdrawal.generation)
            }
        )
        .await
        .expect("should reject stale pre-withdrawal CAS"),
        EcKvWriteOutcome::PreconditionFailed
    );
    assert!(
        response
            .headers()
            .get_all("set-cookie")
            .iter()
            .any(|value| value
                .to_str()
                .is_ok_and(|value| value.starts_with("ts-ec=;") && value.contains("Max-Age=0"))),
        "withdrawal should expire browser EC cookie"
    );
}
