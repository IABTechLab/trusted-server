use std::collections::HashMap;

use base64::{Engine as _, engine::general_purpose};
use error_stack::{Report, ResultExt};
use reqwest::blocking::{Client, Response};
use serde::{Deserialize, Serialize};

use crate::error::CliError;

const FASTLY_API_BASE: &str = "https://api.fastly.com";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NamedResource {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceVersion {
    pub number: u32,
    pub active: bool,
    pub locked: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResourceLink {
    pub id: String,
    pub name: String,
    pub resource_id: String,
}

pub trait FastlyApi {
    fn find_config_store_by_name(
        &self,
        name: &str,
    ) -> Result<Option<NamedResource>, Report<CliError>>;
    fn create_config_store(&self, name: &str) -> Result<NamedResource, Report<CliError>>;
    fn list_config_store_items(
        &self,
        store_id: &str,
    ) -> Result<HashMap<String, String>, Report<CliError>>;
    fn upsert_config_item(
        &self,
        store_id: &str,
        key: &str,
        value: &str,
    ) -> Result<(), Report<CliError>>;

    fn find_secret_store_by_name(
        &self,
        name: &str,
    ) -> Result<Option<NamedResource>, Report<CliError>>;
    fn create_secret_store(&self, name: &str) -> Result<NamedResource, Report<CliError>>;
    fn list_secret_names(&self, store_id: &str) -> Result<Vec<String>, Report<CliError>>;
    fn recreate_secret(
        &self,
        store_id: &str,
        name: &str,
        value: &str,
    ) -> Result<(), Report<CliError>>;

    fn find_kv_store_by_name(&self, name: &str) -> Result<Option<NamedResource>, Report<CliError>>;
    fn create_kv_store(&self, name: &str) -> Result<NamedResource, Report<CliError>>;

    fn list_service_versions(
        &self,
        service_id: &str,
    ) -> Result<Vec<ServiceVersion>, Report<CliError>>;
    fn clone_service_version(
        &self,
        service_id: &str,
        version_number: u32,
    ) -> Result<ServiceVersion, Report<CliError>>;
    fn activate_service_version(
        &self,
        service_id: &str,
        version_number: u32,
    ) -> Result<ServiceVersion, Report<CliError>>;
    fn list_resource_links(
        &self,
        service_id: &str,
        version_number: u32,
    ) -> Result<Vec<ResourceLink>, Report<CliError>>;
    fn create_resource_link(
        &self,
        service_id: &str,
        version_number: u32,
        resource_id: &str,
        name: &str,
    ) -> Result<ResourceLink, Report<CliError>>;
    fn update_resource_link(
        &self,
        service_id: &str,
        version_number: u32,
        link_id: &str,
        resource_id: &str,
        name: &str,
    ) -> Result<ResourceLink, Report<CliError>>;
}

pub struct ReqwestFastlyApi {
    client: Client,
    api_key: String,
}

impl ReqwestFastlyApi {
    pub fn new(api_key: String) -> Result<Self, Report<CliError>> {
        let client = Client::builder()
            .user_agent("trusted-server-cli/0.1")
            .build()
            .change_context(CliError::FastlyApi)?;
        Ok(Self { client, api_key })
    }

    fn request(&self, method: reqwest::Method, path: &str) -> reqwest::blocking::RequestBuilder {
        self.client
            .request(method, format!("{FASTLY_API_BASE}{path}"))
            .header("Fastly-Key", &self.api_key)
            .header("Accept", "application/json")
    }

    fn ensure_success(
        &self,
        response: Response,
        context: &str,
    ) -> Result<Response, Report<CliError>> {
        let status = response.status();
        if status.is_success() {
            return Ok(response);
        }

        let body = response
            .text()
            .unwrap_or_else(|_| "<unreadable body>".to_string());
        Err(Report::new(CliError::FastlyApi)
            .attach(format!("{context} failed with HTTP {status}: {body}")))
    }
}

impl FastlyApi for ReqwestFastlyApi {
    fn find_config_store_by_name(
        &self,
        name: &str,
    ) -> Result<Option<NamedResource>, Report<CliError>> {
        let response = self
            .request(reqwest::Method::GET, "/resources/stores/config")
            .query(&[("name", name)])
            .send()
            .change_context(CliError::FastlyApi)?;
        let response = self.ensure_success(response, "listing config stores")?;
        let stores: Vec<NamedResource> = response.json().change_context(CliError::FastlyApi)?;
        Ok(stores.into_iter().next())
    }

    fn create_config_store(&self, name: &str) -> Result<NamedResource, Report<CliError>> {
        let response = self
            .request(reqwest::Method::POST, "/resources/stores/config")
            .form(&[("name", name)])
            .send()
            .change_context(CliError::FastlyApi)?;
        let response = self.ensure_success(response, "creating config store")?;
        response.json().change_context(CliError::FastlyApi)
    }

    fn list_config_store_items(
        &self,
        store_id: &str,
    ) -> Result<HashMap<String, String>, Report<CliError>> {
        let response = self
            .request(
                reqwest::Method::GET,
                &format!("/resources/stores/config/{store_id}/items"),
            )
            .send()
            .change_context(CliError::FastlyApi)?;
        let response = self.ensure_success(response, "listing config store items")?;
        let items: Vec<ConfigStoreItemResponse> =
            response.json().change_context(CliError::FastlyApi)?;
        Ok(items
            .into_iter()
            .map(|item| (item.item_key, item.item_value))
            .collect())
    }

    fn upsert_config_item(
        &self,
        store_id: &str,
        key: &str,
        value: &str,
    ) -> Result<(), Report<CliError>> {
        let response = self
            .request(
                reqwest::Method::PUT,
                &format!("/resources/stores/config/{store_id}/item/{key}"),
            )
            .form(&[("item_key", key), ("item_value", value)])
            .send()
            .change_context(CliError::FastlyApi)?;
        self.ensure_success(response, "upserting config store item")?;
        Ok(())
    }

    fn find_secret_store_by_name(
        &self,
        name: &str,
    ) -> Result<Option<NamedResource>, Report<CliError>> {
        let response = self
            .request(reqwest::Method::GET, "/resources/stores/secret")
            .query(&[("name", name), ("limit", "200")])
            .send()
            .change_context(CliError::FastlyApi)?;
        let response = self.ensure_success(response, "listing secret stores")?;
        let listing: SecretStoreListing = response.json().change_context(CliError::FastlyApi)?;
        Ok(listing.data.into_iter().next().map(|store| NamedResource {
            id: store.id,
            name: store.name,
        }))
    }

    fn create_secret_store(&self, name: &str) -> Result<NamedResource, Report<CliError>> {
        let response = self
            .request(reqwest::Method::POST, "/resources/stores/secret")
            .json(&serde_json::json!({ "name": name }))
            .send()
            .change_context(CliError::FastlyApi)?;
        let response = self.ensure_success(response, "creating secret store")?;
        let store: SecretStoreRecord = response.json().change_context(CliError::FastlyApi)?;
        Ok(NamedResource {
            id: store.id,
            name: store.name,
        })
    }

    fn list_secret_names(&self, store_id: &str) -> Result<Vec<String>, Report<CliError>> {
        let response = self
            .request(
                reqwest::Method::GET,
                &format!("/resources/stores/secret/{store_id}/secrets"),
            )
            .query(&[("limit", "200")])
            .send()
            .change_context(CliError::FastlyApi)?;
        let response = self.ensure_success(response, "listing secret store secrets")?;
        let listing: SecretItemListing = response.json().change_context(CliError::FastlyApi)?;
        Ok(listing.data.into_iter().map(|secret| secret.name).collect())
    }

    fn recreate_secret(
        &self,
        store_id: &str,
        name: &str,
        value: &str,
    ) -> Result<(), Report<CliError>> {
        let encoded = general_purpose::STANDARD.encode(value.as_bytes());
        let response = self
            .request(
                reqwest::Method::PUT,
                &format!("/resources/stores/secret/{store_id}/secrets"),
            )
            .json(&serde_json::json!({ "name": name, "secret": encoded }))
            .send()
            .change_context(CliError::FastlyApi)?;
        self.ensure_success(response, "recreating secret")?;
        Ok(())
    }

    fn find_kv_store_by_name(&self, name: &str) -> Result<Option<NamedResource>, Report<CliError>> {
        let response = self
            .request(reqwest::Method::GET, "/resources/stores/kv")
            .query(&[("name", name), ("limit", "1000")])
            .send()
            .change_context(CliError::FastlyApi)?;
        let response = self.ensure_success(response, "listing KV stores")?;
        let listing: KvStoreListing = response.json().change_context(CliError::FastlyApi)?;
        Ok(listing.data.into_iter().next().map(|store| NamedResource {
            id: store.id,
            name: store.name,
        }))
    }

    fn create_kv_store(&self, name: &str) -> Result<NamedResource, Report<CliError>> {
        let response = self
            .request(reqwest::Method::POST, "/resources/stores/kv")
            .query(&[("location", "US")])
            .json(&serde_json::json!({ "name": name }))
            .send()
            .change_context(CliError::FastlyApi)?;
        let response = self.ensure_success(response, "creating KV store")?;
        let store: KvStoreRecord = response.json().change_context(CliError::FastlyApi)?;
        Ok(NamedResource {
            id: store.id,
            name: store.name,
        })
    }

    fn list_service_versions(
        &self,
        service_id: &str,
    ) -> Result<Vec<ServiceVersion>, Report<CliError>> {
        let response = self
            .request(
                reqwest::Method::GET,
                &format!("/service/{service_id}/version"),
            )
            .send()
            .change_context(CliError::FastlyApi)?;
        let response = self.ensure_success(response, "listing service versions")?;
        response.json().change_context(CliError::FastlyApi)
    }

    fn clone_service_version(
        &self,
        service_id: &str,
        version_number: u32,
    ) -> Result<ServiceVersion, Report<CliError>> {
        let response = self
            .request(
                reqwest::Method::PUT,
                &format!("/service/{service_id}/version/{version_number}/clone"),
            )
            .send()
            .change_context(CliError::FastlyApi)?;
        let response = self.ensure_success(response, "cloning service version")?;
        response.json().change_context(CliError::FastlyApi)
    }

    fn activate_service_version(
        &self,
        service_id: &str,
        version_number: u32,
    ) -> Result<ServiceVersion, Report<CliError>> {
        let response = self
            .request(
                reqwest::Method::PUT,
                &format!("/service/{service_id}/version/{version_number}/activate"),
            )
            .send()
            .change_context(CliError::FastlyApi)?;
        let response = self.ensure_success(response, "activating service version")?;
        response.json().change_context(CliError::FastlyApi)
    }

    fn list_resource_links(
        &self,
        service_id: &str,
        version_number: u32,
    ) -> Result<Vec<ResourceLink>, Report<CliError>> {
        let response = self
            .request(
                reqwest::Method::GET,
                &format!("/service/{service_id}/version/{version_number}/resource"),
            )
            .send()
            .change_context(CliError::FastlyApi)?;
        let response = self.ensure_success(response, "listing resource links")?;
        response.json().change_context(CliError::FastlyApi)
    }

    fn create_resource_link(
        &self,
        service_id: &str,
        version_number: u32,
        resource_id: &str,
        name: &str,
    ) -> Result<ResourceLink, Report<CliError>> {
        let response = self
            .request(
                reqwest::Method::POST,
                &format!("/service/{service_id}/version/{version_number}/resource"),
            )
            .form(&[("resource_id", resource_id), ("name", name)])
            .send()
            .change_context(CliError::FastlyApi)?;
        let response = self.ensure_success(response, "creating resource link")?;
        response.json().change_context(CliError::FastlyApi)
    }

    fn update_resource_link(
        &self,
        service_id: &str,
        version_number: u32,
        link_id: &str,
        resource_id: &str,
        name: &str,
    ) -> Result<ResourceLink, Report<CliError>> {
        let response = self
            .request(
                reqwest::Method::PUT,
                &format!("/service/{service_id}/version/{version_number}/resource/{link_id}"),
            )
            .form(&[("resource_id", resource_id), ("name", name)])
            .send()
            .change_context(CliError::FastlyApi)?;
        let response = self.ensure_success(response, "updating resource link")?;
        response.json().change_context(CliError::FastlyApi)
    }
}

#[derive(Debug, Deserialize)]
struct ConfigStoreItemResponse {
    item_key: String,
    item_value: String,
}

#[derive(Debug, Deserialize)]
struct SecretStoreListing {
    #[serde(default)]
    data: Vec<SecretStoreRecord>,
}

#[derive(Debug, Deserialize)]
struct SecretStoreRecord {
    id: String,
    name: String,
}

#[derive(Debug, Deserialize)]
struct SecretItemListing {
    #[serde(default)]
    data: Vec<SecretItemRecord>,
}

#[derive(Debug, Deserialize)]
struct SecretItemRecord {
    name: String,
}

#[derive(Debug, Deserialize)]
struct KvStoreListing {
    #[serde(default)]
    data: Vec<KvStoreRecord>,
}

#[derive(Debug, Deserialize)]
struct KvStoreRecord {
    id: String,
    name: String,
}
