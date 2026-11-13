//! Native single-region DynamoDB storage for the EC graph.
//!
//! Rows and the retained generation allocator use separate namespace partitions.
//! Never delete or expire the allocator while a namespace can be reused: its
//! monotonic counter prevents stale CAS after hard deletion and recreation.
//! Lookup/list apply logical TTL. Exact existence includes expired physical rows
//! so withdrawal can fence conditional writes already in flight before expiry.

use std::sync::Arc;

use async_trait::async_trait;
use edgezero_core::EdgeError;
use error_stack::Report;
use trusted_server_core::ec::kv_backend::{EcKvLookup, EcKvStore, EcKvWrite, EcKvWriteOutcome};
use trusted_server_core::ec::storage::DynamoDbEcStorage;
use trusted_server_core::error::TrustedServerError;

/// Cloneable live provider. Clones share the SDK connection pool, not a cache.
#[derive(Clone, Debug)]
pub struct DynamoDbEcKv {
    #[cfg(feature = "aws-dynamodb-ec")]
    client: aws_sdk_dynamodb::Client,
    #[cfg(feature = "aws-dynamodb-ec")]
    table: String,
    #[cfg(feature = "aws-dynamodb-ec")]
    partition: String,
    #[cfg(feature = "aws-dynamodb-ec")]
    allocator_partition: String,
}

fn preparation_error() -> EdgeError {
    EdgeError::internal(std::io::Error::other(
        "native EC DynamoDB preparation failed",
    ))
}

fn store_error() -> Report<TrustedServerError> {
    Report::new(TrustedServerError::KvStore {
        store_name: "native-ec-dynamodb".to_owned(),
        message: "EC DynamoDB operation failed".to_owned(),
    })
}

/// Reject obsolete native launch selection without preparing from environment.
///
/// # Errors
/// Enabled or malformed legacy selection must be migrated to published config.
pub fn reject_legacy_configuration() -> Result<(), EdgeError> {
    match std::env::var("TS_EC_DYNAMODB_ENABLED") {
        Err(std::env::VarError::NotPresent) => Ok(()),
        Ok(value) if value == "false" => Ok(()),
        Ok(value) if value == "true" => Err(EdgeError::internal(std::io::Error::other(
            "TS_EC_DYNAMODB_ENABLED is obsolete; publish ec.storage and remove legacy EC environment selection",
        ))),
        _ => Err(EdgeError::internal(std::io::Error::other(
            "TS_EC_DYNAMODB_ENABLED is malformed; migrate to published ec.storage configuration",
        ))),
    }
}

/// Prepare the explicitly selected typed `DynamoDB` provider before readiness.
///
/// # Errors
/// Returns a sanitized error on invalid input, missing feature or failed preparation.
pub async fn prepare(config: &DynamoDbEcStorage) -> Result<Arc<DynamoDbEcKv>, EdgeError> {
    config.validate().map_err(|_| preparation_error())?;
    #[cfg(feature = "aws-dynamodb-ec")]
    {
        enabled::prepare(config).await
    }
    #[cfg(not(feature = "aws-dynamodb-ec"))]
    Err(EdgeError::internal(std::io::Error::other(
        "native EC DynamoDB selected but aws-dynamodb-ec build feature is unavailable",
    )))
}

#[cfg(not(feature = "aws-dynamodb-ec"))]
#[async_trait(?Send)]
impl EcKvStore for DynamoDbEcKv {
    fn store_name(&self) -> &str {
        "native-ec-dynamodb"
    }
    async fn lookup(&self, _: &str) -> Result<Option<EcKvLookup>, Report<TrustedServerError>> {
        Err(store_error())
    }
    async fn key_exists(&self, _: &str) -> Result<bool, Report<TrustedServerError>> {
        Err(store_error())
    }
    async fn insert(
        &self,
        _: &str,
        _: EcKvWrite<'_>,
    ) -> Result<EcKvWriteOutcome, Report<TrustedServerError>> {
        Err(store_error())
    }
    async fn list_keys_with_prefix(
        &self,
        _: &str,
        _: u32,
    ) -> Result<Vec<String>, Report<TrustedServerError>> {
        Err(store_error())
    }
    async fn delete(&self, _: &str) -> Result<(), Report<TrustedServerError>> {
        Err(store_error())
    }
}

#[cfg(feature = "aws-dynamodb-ec")]
mod enabled {
    use std::collections::HashMap;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use aws_config::BehaviorVersion;
    use aws_sdk_dynamodb::config::{Region, retry::RetryConfig, timeout::TimeoutConfig};
    use aws_sdk_dynamodb::primitives::Blob;
    use aws_sdk_dynamodb::types::{AttributeValue, ReturnValue, TableDescription};
    use trusted_server_core::ec::kv_backend::EcKvWriteMode;

    use super::{
        Arc, DynamoDbEcKv, EcKvLookup, EcKvStore, EcKvWrite, EcKvWriteOutcome, EdgeError, Report,
        TrustedServerError, async_trait, preparation_error, store_error,
    };

    // Query can stop at 1 MB or an empty filtered page. These bounds limit work,
    // not completeness: budget exhaustion is an error, never a short success.
    const MAX_LIST_PAGES: usize = 64;
    const PAGE_SIZE: usize = 1_000;
    const OPERATION_TIMEOUT: Duration = Duration::from_secs(3);
    const PREPARATION_TIMEOUT: Duration = Duration::from_secs(10);
    type Item = HashMap<String, AttributeValue>;
    type StoreResult<T> = Result<T, Report<TrustedServerError>>;

    pub(super) async fn prepare(
        config: &super::DynamoDbEcStorage,
    ) -> Result<Arc<DynamoDbEcKv>, EdgeError> {
        tokio::time::timeout(PREPARATION_TIMEOUT, async {
            let builder = match &config.local_endpoint {
                Some(endpoint) => {
                    // Explicit emulator mode never reads ambient credentials.
                    aws_sdk_dynamodb::Config::builder()
                        .behavior_version(BehaviorVersion::v2026_01_12())
                        .region(Region::new(config.region.clone()))
                        .credentials_provider(aws_sdk_dynamodb::config::Credentials::new(
                            "local",
                            "local",
                            None,
                            None,
                            "ec-dynamodb-local",
                        ))
                        .endpoint_url(endpoint)
                }
                None => {
                    let shared = aws_config::defaults(BehaviorVersion::v2026_01_12())
                        .region(Region::new(config.region.clone()))
                        .load()
                        .await;
                    let mut builder = aws_sdk_dynamodb::Config::from(&shared).to_builder();
                    // Production ignores ambient endpoint overrides.
                    builder.set_endpoint_url(None);
                    builder
                }
            };
            let config_sdk = builder
                .retry_config(RetryConfig::standard().with_max_attempts(1))
                .timeout_config(timeout_config())
                .build();
            DynamoDbEcKv::prepare_with_client(
                aws_sdk_dynamodb::Client::from_conf(config_sdk),
                config.table.clone(),
                config.namespace.clone(),
            )
            .await
        })
        .await
        .map_err(|_| preparation_error())?
    }

    fn validate_binding(table: &str, namespace: &str) -> Result<(), EdgeError> {
        if !(3..=255).contains(&table.len())
            || !table
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b))
            || namespace.is_empty()
            || namespace.len() > 512
            || namespace.chars().any(char::is_control)
        {
            return Err(preparation_error());
        }
        Ok(())
    }

    fn timeout_config() -> TimeoutConfig {
        TimeoutConfig::builder()
            .connect_timeout(Duration::from_secs(1))
            .operation_attempt_timeout(OPERATION_TIMEOUT)
            .operation_timeout(OPERATION_TIMEOUT)
            .build()
    }

    impl DynamoDbEcKv {
        /// Prepare a provider with an explicitly supplied SDK client.
        ///
        /// Useful for `DynamoDB` Local and deterministic loopback tests without
        /// reading ambient credentials or changing process environment. The
        /// caller owns endpoint/credential configuration; retries are disabled
        /// for each operation even if the supplied client enables them.
        ///
        /// # Errors
        /// Returns a sanitized error when bindings or the table are unsuitable.
        pub async fn prepare_with_client(
            client: aws_sdk_dynamodb::Client,
            table: impl Into<String>,
            namespace: impl Into<String>,
        ) -> Result<Arc<Self>, EdgeError> {
            let table = table.into();
            let namespace = namespace.into();
            validate_binding(&table, &namespace)?;
            let provider = Self {
                client,
                table,
                partition: format!("ec:{namespace}"),
                allocator_partition: format!("ec-allocator:{namespace}"),
            };
            let output = tokio::time::timeout(
                OPERATION_TIMEOUT,
                provider
                    .client
                    .describe_table()
                    .table_name(&provider.table)
                    .customize()
                    .config_override(Self::policy())
                    .send(),
            )
            .await
            .map_err(|_| preparation_error())?
            .map_err(|_| preparation_error())?;
            validate_table(
                output.table.as_ref().ok_or_else(preparation_error)?,
                &provider.table,
            )?;
            Ok(Arc::new(provider))
        }

        fn policy() -> aws_sdk_dynamodb::config::Builder {
            aws_sdk_dynamodb::Config::builder()
                .retry_config(RetryConfig::standard().with_max_attempts(1))
                .timeout_config(timeout_config())
        }

        fn key(&self, key: &str) -> Item {
            HashMap::from([
                ("pk".to_owned(), AttributeValue::S(self.partition.clone())),
                ("sk".to_owned(), AttributeValue::S(key.to_owned())),
            ])
        }

        async fn allocate_generation(&self) -> StoreResult<u64> {
            let output = self
                .client
                .update_item()
                .table_name(&self.table)
                .key("pk", AttributeValue::S(self.allocator_partition.clone()))
                .key("sk", AttributeValue::S("generation".to_owned()))
                .update_expression("ADD #g :one")
                .condition_expression("attribute_not_exists(#g) OR #g < :max")
                .expression_attribute_names("#g", "generation")
                .expression_attribute_values(":one", AttributeValue::N("1".to_owned()))
                .expression_attribute_values(":max", AttributeValue::N(u64::MAX.to_string()))
                .return_values(ReturnValue::UpdatedNew)
                .customize()
                .config_override(Self::policy())
                .send()
                .await
                .map_err(|_| store_error())?;
            let generation = unsigned(
                output.attributes.as_ref().ok_or_else(store_error)?,
                "generation",
            )?;
            if generation == 0 {
                return Err(store_error());
            }
            Ok(generation)
        }

        async fn get_physical(&self, key: &str) -> StoreResult<Option<Item>> {
            validate_key(key)?;
            let output = self
                .client
                .get_item()
                .table_name(&self.table)
                .set_key(Some(self.key(key)))
                .consistent_read(true)
                .customize()
                .config_override(Self::policy())
                .send()
                .await
                .map_err(|_| store_error())?;
            match output.item {
                None => Ok(None),
                Some(item) => {
                    if row_key(&item, &self.partition)? != key {
                        return Err(store_error());
                    }
                    // Malformed expiry is an error, not evidence of absence.
                    expiry(&item)?;
                    Ok(Some(item))
                }
            }
        }
    }

    #[async_trait(?Send)]
    impl EcKvStore for DynamoDbEcKv {
        fn store_name(&self) -> &str {
            "native-ec-dynamodb"
        }

        async fn lookup(&self, key: &str) -> StoreResult<Option<EcKvLookup>> {
            tokio::time::timeout(OPERATION_TIMEOUT, async {
                let Some(item) = self.get_physical(key).await? else {
                    return Ok(None);
                };
                if expiry(&item)? <= now_ms()? {
                    return Ok(None);
                }
                let binary = |name: &str| match item.get(name) {
                    Some(AttributeValue::B(value)) => Ok(value.as_ref().to_vec()),
                    _ => Err(store_error()),
                };
                let generation = unsigned(&item, "generation")?;
                if generation == 0 {
                    return Err(store_error());
                }
                Ok(Some(EcKvLookup {
                    body: binary("body")?,
                    metadata: Some(binary("metadata")?),
                    generation,
                }))
            })
            .await
            .map_err(|_| store_error())?
        }

        async fn key_exists(&self, key: &str) -> StoreResult<bool> {
            // Withdrawal must fence writes already in flight with an earlier
            // client timestamp. An expired physical row can still satisfy such
            // a CAS, so only physical absence proves there is nothing to revoke.
            tokio::time::timeout(OPERATION_TIMEOUT, self.get_physical(key))
                .await
                .map_err(|_| store_error())?
                .map(|item| item.is_some())
        }

        async fn insert(&self, key: &str, write: EcKvWrite<'_>) -> StoreResult<EcKvWriteOutcome> {
            validate_key(key)?;
            // DynamoDB caps complete items at 400 KiB. Include all names,
            // partition/key bytes and conservative decimal numeric accounting.
            let bytes = write
                .body
                .len()
                .checked_add(write.metadata.len())
                .and_then(|n| n.checked_add(self.partition.len() + key.len() + 256))
                .ok_or_else(store_error)?;
            if bytes > 409_600 || write.ttl.is_zero() {
                return Err(store_error());
            }
            tokio::time::timeout(OPERATION_TIMEOUT, async {
                let generation = self.allocate_generation().await?;
                let timestamp = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map_err(|_| store_error())?;
                let now = u64::try_from(timestamp.as_millis()).map_err(|_| store_error())?;
                let expires = expiry_from_duration(timestamp, write.ttl)?;
                let mut item = self.key(key);
                item.insert(
                    "body".to_owned(),
                    AttributeValue::B(Blob::new(write.body.as_bytes())),
                );
                item.insert(
                    "metadata".to_owned(),
                    AttributeValue::B(Blob::new(write.metadata.as_bytes())),
                );
                item.insert(
                    "generation".to_owned(),
                    AttributeValue::N(generation.to_string()),
                );
                item.insert(
                    "expires_at_ms".to_owned(),
                    AttributeValue::N(expires.to_string()),
                );
                item.insert(
                    "ttl_epoch_s".to_owned(),
                    AttributeValue::N(expires.div_ceil(1_000).to_string()),
                );
                let mut request = self
                    .client
                    .put_item()
                    .table_name(&self.table)
                    .set_item(Some(item));
                request = match write.mode {
                    EcKvWriteMode::Add => request
                        .condition_expression("attribute_not_exists(#pk) OR #e <= :now")
                        .expression_attribute_names("#pk", "pk")
                        .expression_attribute_names("#e", "expires_at_ms")
                        .expression_attribute_values(":now", AttributeValue::N(now.to_string())),
                    EcKvWriteMode::IfGenerationMatch(expected) => request
                        .condition_expression("#g = :expected AND #e > :now")
                        .expression_attribute_names("#g", "generation")
                        .expression_attribute_names("#e", "expires_at_ms")
                        .expression_attribute_values(
                            ":expected",
                            AttributeValue::N(expected.to_string()),
                        )
                        .expression_attribute_values(":now", AttributeValue::N(now.to_string())),
                    EcKvWriteMode::Overwrite => request,
                };
                match request
                    .customize()
                    .config_override(Self::policy())
                    .send()
                    .await
                {
                    Ok(_) => Ok(EcKvWriteOutcome::Written),
                    Err(error)
                        if error
                            .as_service_error()
                            .is_some_and(aws_sdk_dynamodb::operation::put_item::PutItemError::is_conditional_check_failed_exception) =>
                    {
                        Ok(EcKvWriteOutcome::PreconditionFailed)
                    }
                    Err(_) => Err(store_error()),
                }
            })
            .await
            .map_err(|_| store_error())?
        }

        async fn list_keys_with_prefix(
            &self,
            prefix: &str,
            limit: u32,
        ) -> StoreResult<Vec<String>> {
            if prefix.len() > 1_024 || prefix.chars().any(char::is_control) {
                return Err(store_error());
            }
            if limit == 0 {
                return Ok(Vec::new());
            }
            tokio::time::timeout(OPERATION_TIMEOUT, async {
                let mut keys = Vec::new();
                let mut cursor: Option<Item> = None;
                for _ in 0..MAX_LIST_PAGES {
                    let remaining = (limit as usize).saturating_sub(keys.len()).min(PAGE_SIZE);
                    let mut request = self
                        .client
                        .query()
                        .table_name(&self.table)
                        .consistent_read(true)
                        .limit(i32::try_from(remaining).map_err(|_| store_error())?)
                        .key_condition_expression(if prefix.is_empty() {
                            "#pk = :pk"
                        } else {
                            "#pk = :pk AND begins_with(#sk, :prefix)"
                        })
                        .expression_attribute_names("#pk", "pk")
                        .expression_attribute_names("#sk", "sk")
                        .expression_attribute_names("#e", "expires_at_ms")
                        .expression_attribute_names("#t", "ttl_epoch_s")
                        .expression_attribute_values(
                            ":pk",
                            AttributeValue::S(self.partition.clone()),
                        )
                        .projection_expression("#pk, #sk, #e, #t")
                        .set_exclusive_start_key(cursor.clone());
                    if !prefix.is_empty() {
                        request = request.expression_attribute_values(
                            ":prefix",
                            AttributeValue::S(prefix.to_owned()),
                        );
                    }
                    let output = request
                        .customize()
                        .config_override(Self::policy())
                        .send()
                        .await
                        .map_err(|_| store_error())?;
                    let items = output.items.unwrap_or_default();
                    if items.len() > remaining {
                        return Err(store_error());
                    }
                    let mut previous = cursor
                        .as_ref()
                        .map(|c| row_key(c, &self.partition))
                        .transpose()?
                        .map(str::to_owned);
                    let now = now_ms()?;
                    for item in &items {
                        let key = row_key(item, &self.partition)?;
                        if !key.starts_with(prefix)
                            || previous.as_deref().is_some_and(|last| key <= last)
                        {
                            return Err(store_error());
                        }
                        previous = Some(key.to_owned());
                        if expiry(item)? > now {
                            keys.push(key.to_owned());
                        }
                    }
                    let next = output.last_evaluated_key.filter(|c| !c.is_empty());
                    if let Some(next) = &next {
                        let key = row_key(next, &self.partition)?;
                        if next.len() != 2
                            || !key.starts_with(prefix)
                            || cursor.as_ref().is_some_and(|last| {
                                row_key(last, &self.partition).is_ok_and(|last| key <= last)
                            })
                            || previous.as_deref().is_some_and(|last| key < last)
                        {
                            return Err(store_error());
                        }
                    }
                    if keys.len() == limit as usize || next.is_none() {
                        return Ok(keys);
                    }
                    cursor = next;
                }
                Err(store_error())
            })
            .await
            .map_err(|_| store_error())?
        }

        async fn delete(&self, key: &str) -> StoreResult<()> {
            validate_key(key)?;
            tokio::time::timeout(
                OPERATION_TIMEOUT,
                self.client
                    .delete_item()
                    .table_name(&self.table)
                    .set_key(Some(self.key(key)))
                    .customize()
                    .config_override(Self::policy())
                    .send(),
            )
            .await
            .map_err(|_| store_error())?
            .map_err(|_| store_error())?;
            Ok(())
        }
    }

    fn validate_key(key: &str) -> StoreResult<()> {
        if key.is_empty() || key.len() > 1_024 || key.chars().any(char::is_control) {
            return Err(store_error());
        }
        Ok(())
    }

    fn now_ms() -> StoreResult<u64> {
        u64::try_from(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(|_| store_error())?
                .as_millis(),
        )
        .map_err(|_| store_error())
    }

    fn expiry_from_duration(now: Duration, ttl: Duration) -> StoreResult<u64> {
        let expires = now.checked_add(ttl).ok_or_else(store_error)?;
        let rounded = expires
            .as_millis()
            .checked_add(u128::from(
                !expires.subsec_nanos().is_multiple_of(1_000_000),
            ))
            .ok_or_else(store_error)?;
        u64::try_from(rounded).map_err(|_| store_error())
    }

    fn unsigned(item: &Item, name: &str) -> StoreResult<u64> {
        let Some(AttributeValue::N(value)) = item.get(name) else {
            return Err(store_error());
        };
        let number = value.parse::<u64>().map_err(|_| store_error())?;
        if number.to_string() != *value {
            return Err(store_error());
        }
        Ok(number)
    }

    fn expiry(item: &Item) -> StoreResult<u64> {
        let expires = unsigned(item, "expires_at_ms")?;
        if unsigned(item, "ttl_epoch_s")? != expires.div_ceil(1_000) {
            return Err(store_error());
        }
        Ok(expires)
    }

    fn row_key<'a>(item: &'a Item, partition: &str) -> StoreResult<&'a str> {
        match (item.get("pk"), item.get("sk")) {
            (Some(AttributeValue::S(pk)), Some(AttributeValue::S(sk))) if pk == partition => {
                validate_key(sk)?;
                Ok(sk)
            }
            _ => Err(store_error()),
        }
    }

    fn validate_table(table: &TableDescription, expected: &str) -> Result<(), EdgeError> {
        if table.table_name.as_deref() != Some(expected)
            || table
                .table_status
                .as_ref()
                .is_none_or(|status| status.as_str() != "ACTIVE")
            || !table.replicas().is_empty()
            || table.global_table_version.is_some()
            || !table.global_secondary_indexes().is_empty()
            || !table.local_secondary_indexes().is_empty()
            || table
                .vector_indexes
                .as_ref()
                .is_some_and(|indexes| !indexes.is_empty())
            || table.key_schema().len() != 2
            || table.attribute_definitions().len() != 2
            || ![("pk", "HASH"), ("sk", "RANGE")]
                .into_iter()
                .all(|(name, kind)| {
                    table
                        .key_schema()
                        .iter()
                        .any(|key| key.attribute_name() == name && key.key_type().as_str() == kind)
                })
            || !["pk", "sk"].into_iter().all(|name| {
                table.attribute_definitions().iter().any(|attr| {
                    attr.attribute_name() == name && attr.attribute_type().as_str() == "S"
                })
            })
        {
            return Err(preparation_error());
        }
        Ok(())
    }
    #[cfg(test)]
    mod unit_tests {
        use super::*;

        #[test]
        fn absolute_expiry_rounding_never_shortens_ttl() {
            assert_eq!(
                expiry_from_duration(Duration::from_nanos(999_999), Duration::from_nanos(1))
                    .expect("should round exact millisecond boundary"),
                1
            );
            assert_eq!(
                expiry_from_duration(Duration::from_nanos(999_999), Duration::from_nanos(2))
                    .expect("should round above millisecond boundary"),
                2
            );
            assert_eq!(
                expiry_from_duration(Duration::from_nanos(1), Duration::from_secs(60))
                    .expect("should retain fractional current timestamp"),
                60_001
            );
            assert!(expiry_from_duration(Duration::MAX, Duration::from_nanos(1)).is_err());
            assert!(expiry_from_duration(Duration::ZERO, Duration::MAX).is_err());
        }
    }
}

#[cfg(all(test, feature = "aws-dynamodb-ec"))]
#[path = "ec_kv_tests.rs"]
mod tests;
