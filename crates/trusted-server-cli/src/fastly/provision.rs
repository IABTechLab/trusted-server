use std::collections::HashMap;

use base64::{Engine as _, engine::general_purpose};
use dialoguer::Confirm;
use error_stack::{Report, ResultExt};
use serde::Serialize;
use trusted_server_core::request_signing::{
    JWKS_CONFIG_STORE_NAME, Keypair, SIGNING_SECRET_STORE_NAME,
};
use trusted_server_core::runtime_config::{APPLICATION_CONFIG_KEY, APPLICATION_CONFIG_STORE_NAME};
use uuid::Uuid;

use crate::config::ValidatedConfig;
use crate::error::CliError;
use crate::fastly::api::{FastlyApi, NamedResource, ResourceLink};

const FASTLY_API_SECRET_STORE_NAME: &str = "api-keys";
const FASTLY_API_SECRET_KEY: &str = "api_key";

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ResourceKind {
    Config,
    Secret,
    Kv,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ChangeKind {
    Create,
    Update,
    Bind,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ProvisionActionJson {
    pub action: ChangeKind,
    pub resource_kind: ResourceKind,
    pub name: String,
    pub detail: String,
    pub remote_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ServiceVersionPlanJson {
    pub latest_version: u32,
    pub target_version: u32,
    pub clone_required: bool,
    pub clone_source_version: Option<u32>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ProvisionPlanJson {
    pub service_id: String,
    pub config_path: String,
    pub service_version: ServiceVersionPlanJson,
    pub actions: Vec<ProvisionActionJson>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ProvisionApplyJson {
    pub service_id: String,
    pub config_path: String,
    pub service_version: ServiceVersionPlanJson,
    pub completed_actions: Vec<ProvisionActionJson>,
    pub warnings: Vec<String>,
    pub failed_action: Option<ProvisionActionJson>,
    pub activated_version: bool,
}

#[derive(Debug, Clone)]
pub struct ProvisionPlan {
    pub json: ProvisionPlanJson,
    resources: Vec<PlannedResource>,
}

#[derive(Debug, Clone)]
struct PlannedResource {
    kind: ResourceKind,
    name: String,
    existing_id: Option<String>,
    create_store: bool,
    config_items: Vec<ConfigItemPlan>,
    secrets: Vec<SecretPlan>,
    link: Option<LinkPlan>,
}

#[derive(Debug, Clone)]
struct ConfigItemPlan {
    key: String,
    value: String,
    action: Option<ChangeKind>,
}

#[derive(Debug, Clone)]
struct SecretPlan {
    name: String,
    value: SecretValuePlan,
    action: Option<ChangeKind>,
}

#[derive(Debug, Clone)]
enum SecretValuePlan {
    Literal(String),
    RuntimeApiKey,
}

#[derive(Debug, Clone)]
struct LinkPlan {
    existing_link_id: Option<String>,
    action: Option<ChangeKind>,
}

#[derive(Debug, Clone)]
struct RequestSigningBootstrap {
    kid: String,
    jwk_json: String,
    private_key_base64: String,
}

#[derive(Debug, Clone)]
struct PlannedRequestSigningResources {
    resources: Vec<PlannedResource>,
    bootstrap_planned: bool,
    runtime_api_key_required: bool,
}

pub fn plan_fastly_provisioning(
    api: &dyn FastlyApi,
    validated: &ValidatedConfig,
    service_id: &str,
) -> Result<ProvisionPlan, Report<CliError>> {
    let versions = api.list_service_versions(service_id)?;
    let latest_version = versions
        .iter()
        .max_by_key(|version| version.number)
        .cloned()
        .ok_or_else(|| Report::new(CliError::Provisioning).attach("service has no versions"))?;
    let existing_links = api.list_resource_links(service_id, latest_version.number)?;

    let mut resources = vec![plan_app_config_resource(api, validated, &existing_links)?];
    let mut warnings = Vec::new();

    if let Some(request_signing) = validated.loaded.settings.request_signing.as_ref()
        && request_signing.enabled
    {
        let request_signing_plan = plan_request_signing_resources(api, &existing_links)?;
        if request_signing_plan.bootstrap_planned {
            warnings.push(
                "request signing stores are uninitialized; apply will generate and upload an initial Ed25519 signing keypair"
                    .to_string(),
            );
        }
        if request_signing_plan.runtime_api_key_required {
            warnings.push(
                "request signing requires a runtime Fastly API token for the `api-keys/api_key` secret; apply must be given `FASTLY_RUNTIME_API_KEY`, `--runtime-api-key`, or `--reuse-management-api-key`"
                    .to_string(),
            );
        }
        resources.extend(request_signing_plan.resources);
        append_request_signing_warnings(
            &mut warnings,
            &resources,
            &request_signing.config_store_id,
            &request_signing.secret_store_id,
        );
    }

    if let Some(consent_store) = validated.loaded.settings.consent.consent_store.as_deref() {
        resources.push(plan_kv_resource(api, consent_store, &existing_links)?);
    }

    let requires_binding_change = binding_changes_required(&resources);
    let clone_required = requires_binding_change && latest_version.locked;
    let actions = collect_actions(&resources);

    if clone_required {
        warnings.push(format!(
            "latest service version {} is locked; apply will clone it before creating or updating bindings",
            latest_version.number
        ));
    }
    if requires_binding_change {
        warnings.push(format!(
            "apply will activate service version {} after updating resource bindings",
            latest_version.number
        ));
    }

    Ok(ProvisionPlan {
        json: ProvisionPlanJson {
            service_id: service_id.to_string(),
            config_path: validated.path.display().to_string(),
            service_version: ServiceVersionPlanJson {
                latest_version: latest_version.number,
                target_version: latest_version.number,
                clone_required,
                clone_source_version: clone_required.then_some(latest_version.number),
            },
            actions,
            warnings,
        },
        resources,
    })
}

pub fn apply_fastly_provisioning(
    api: &dyn FastlyApi,
    validated: &ValidatedConfig,
    service_id: &str,
    runtime_api_key: Option<&str>,
    yes: bool,
) -> Result<ProvisionApplyJson, Report<CliError>> {
    let mut plan = plan_fastly_provisioning(api, validated, service_id)?;

    if requires_runtime_api_key(&plan.resources) && runtime_api_key.is_none() {
        return Err(Report::new(CliError::Arguments).attach(
            "request signing provisioning needs a runtime Fastly API token. Set FASTLY_RUNTIME_API_KEY, pass `--runtime-api-key`, or opt in to `--reuse-management-api-key`.",
        ));
    }

    if !yes && !plan.json.actions.is_empty() {
        let confirmed = Confirm::new()
            .with_prompt(format!(
                "Apply {} Fastly provisioning change(s)?",
                plan.json.actions.len()
            ))
            .default(false)
            .interact()
            .change_context(CliError::Cancelled)?;
        if !confirmed {
            return Err(Report::new(CliError::Cancelled).attach("user declined apply"));
        }
    }

    let mut target_version = plan.json.service_version.target_version;
    if plan.json.service_version.clone_required {
        let cloned = api.clone_service_version(service_id, target_version)?;
        target_version = cloned.number;
        plan.json.service_version.target_version = target_version;
    }

    let mut resolved_ids = HashMap::<String, String>::new();
    let mut completed_actions = Vec::new();
    let mut activated_version = false;

    for resource in &plan.resources {
        let mut resource_id = match &resource.existing_id {
            Some(id) => id.clone(),
            None => String::new(),
        };

        if resource.create_store {
            let created = create_store(api, resource)?;
            resource_id = created.id.clone();
            resolved_ids.insert(resource.name.clone(), created.id.clone());
            completed_actions.push(ProvisionActionJson {
                action: ChangeKind::Create,
                resource_kind: resource.kind,
                name: resource.name.clone(),
                detail: format!(
                    "create {} `{}`",
                    resource_kind_label(resource.kind),
                    resource.name
                ),
                remote_id: Some(created.id),
            });
        } else if let Some(existing_id) = &resource.existing_id {
            resolved_ids.insert(resource.name.clone(), existing_id.clone());
        }

        if resource_id.is_empty()
            && let Some(resolved) = resolved_ids.get(&resource.name)
        {
            resource_id = resolved.clone();
        }

        for item in &resource.config_items {
            if let Some(action) = item.action {
                api.upsert_config_item(&resource_id, &item.key, &item.value)?;
                completed_actions.push(ProvisionActionJson {
                    action,
                    resource_kind: resource.kind,
                    name: resource.name.clone(),
                    detail: format!("set config item `{}` in `{}`", item.key, resource.name),
                    remote_id: Some(resource_id.clone()),
                });
            }
        }

        for secret in &resource.secrets {
            if let Some(action) = secret.action {
                let secret_value = match &secret.value {
                    SecretValuePlan::Literal(value) => value.clone(),
                    SecretValuePlan::RuntimeApiKey => runtime_api_key
                        .ok_or_else(|| {
                            Report::new(CliError::Arguments).attach(
                                "missing runtime Fastly API token for request signing provisioning",
                            )
                        })?
                        .to_string(),
                };
                api.recreate_secret(&resource_id, &secret.name, &secret_value)?;
                completed_actions.push(ProvisionActionJson {
                    action,
                    resource_kind: resource.kind,
                    name: resource.name.clone(),
                    detail: format!(
                        "upload secret `{}` to `{}` (value redacted)",
                        secret.name, resource.name
                    ),
                    remote_id: Some(resource_id.clone()),
                });
            }
        }

        if let Some(link) = &resource.link {
            match link.action {
                Some(ChangeKind::Bind) => {
                    api.create_resource_link(
                        service_id,
                        target_version,
                        &resource_id,
                        &resource.name,
                    )?;
                    completed_actions.push(ProvisionActionJson {
                        action: ChangeKind::Bind,
                        resource_kind: resource.kind,
                        name: resource.name.clone(),
                        detail: format!(
                            "bind {} `{}` to service version {}",
                            resource_kind_label(resource.kind),
                            resource.name,
                            target_version
                        ),
                        remote_id: Some(resource_id.clone()),
                    });
                    activated_version = true;
                }
                Some(ChangeKind::Update) => {
                    let link_id = link.existing_link_id.as_deref().ok_or_else(|| {
                        Report::new(CliError::Provisioning).attach("missing resource link ID")
                    })?;
                    api.update_resource_link(
                        service_id,
                        target_version,
                        link_id,
                        &resource_id,
                        &resource.name,
                    )?;
                    completed_actions.push(ProvisionActionJson {
                        action: ChangeKind::Update,
                        resource_kind: resource.kind,
                        name: resource.name.clone(),
                        detail: format!(
                            "update binding for {} `{}` on service version {}",
                            resource_kind_label(resource.kind),
                            resource.name,
                            target_version
                        ),
                        remote_id: Some(resource_id.clone()),
                    });
                    activated_version = true;
                }
                _ => {}
            }
        }
    }

    if activated_version {
        api.activate_service_version(service_id, target_version)?;
    }

    Ok(ProvisionApplyJson {
        service_id: service_id.to_string(),
        config_path: validated.path.display().to_string(),
        service_version: plan.json.service_version,
        completed_actions,
        warnings: plan.json.warnings,
        failed_action: None,
        activated_version,
    })
}

fn plan_app_config_resource(
    api: &dyn FastlyApi,
    validated: &ValidatedConfig,
    existing_links: &[ResourceLink],
) -> Result<PlannedResource, Report<CliError>> {
    let store = api.find_config_store_by_name(APPLICATION_CONFIG_STORE_NAME)?;
    let items = match &store {
        Some(store) => api.list_config_store_items(&store.id)?,
        None => HashMap::new(),
    };

    let action = match items.get(APPLICATION_CONFIG_KEY) {
        Some(existing) if existing == &validated.loaded.canonical_toml => None,
        Some(_) => Some(ChangeKind::Update),
        None => Some(ChangeKind::Create),
    };

    Ok(PlannedResource {
        kind: ResourceKind::Config,
        name: APPLICATION_CONFIG_STORE_NAME.to_string(),
        existing_id: store.as_ref().map(|store| store.id.clone()),
        create_store: store.is_none(),
        config_items: vec![ConfigItemPlan {
            key: APPLICATION_CONFIG_KEY.to_string(),
            value: validated.loaded.canonical_toml.clone(),
            action,
        }],
        secrets: Vec::new(),
        link: Some(plan_link(
            existing_links,
            &store,
            APPLICATION_CONFIG_STORE_NAME,
        )),
    })
}

fn plan_request_signing_resources(
    api: &dyn FastlyApi,
    existing_links: &[ResourceLink],
) -> Result<PlannedRequestSigningResources, Report<CliError>> {
    let config_store = api.find_config_store_by_name(JWKS_CONFIG_STORE_NAME)?;
    let config_items = match &config_store {
        Some(store) => api.list_config_store_items(&store.id)?,
        None => HashMap::new(),
    };

    let signing_secret_store = api.find_secret_store_by_name(SIGNING_SECRET_STORE_NAME)?;
    let signing_secret_names = match &signing_secret_store {
        Some(store) => api.list_secret_names(&store.id)?,
        None => Vec::new(),
    };

    let bootstrap = determine_request_signing_bootstrap(&config_items, &signing_secret_names)?;

    let config_resource = PlannedResource {
        kind: ResourceKind::Config,
        name: JWKS_CONFIG_STORE_NAME.to_string(),
        existing_id: config_store.as_ref().map(|store| store.id.clone()),
        create_store: config_store.is_none(),
        config_items: bootstrap
            .as_ref()
            .map(|bootstrap| {
                vec![
                    ConfigItemPlan {
                        key: "current-kid".to_string(),
                        value: bootstrap.kid.clone(),
                        action: Some(ChangeKind::Create),
                    },
                    ConfigItemPlan {
                        key: "active-kids".to_string(),
                        value: bootstrap.kid.clone(),
                        action: Some(ChangeKind::Create),
                    },
                    ConfigItemPlan {
                        key: bootstrap.kid.clone(),
                        value: bootstrap.jwk_json.clone(),
                        action: Some(ChangeKind::Create),
                    },
                ]
            })
            .unwrap_or_default(),
        secrets: Vec::new(),
        link: Some(plan_link(
            existing_links,
            &config_store,
            JWKS_CONFIG_STORE_NAME,
        )),
    };

    let secret_resource = PlannedResource {
        kind: ResourceKind::Secret,
        name: SIGNING_SECRET_STORE_NAME.to_string(),
        existing_id: signing_secret_store.as_ref().map(|store| store.id.clone()),
        create_store: signing_secret_store.is_none(),
        config_items: Vec::new(),
        secrets: bootstrap
            .as_ref()
            .map(|bootstrap| {
                vec![SecretPlan {
                    name: bootstrap.kid.clone(),
                    value: SecretValuePlan::Literal(bootstrap.private_key_base64.clone()),
                    action: Some(ChangeKind::Create),
                }]
            })
            .unwrap_or_default(),
        link: Some(plan_link(
            existing_links,
            &signing_secret_store,
            SIGNING_SECRET_STORE_NAME,
        )),
    };

    let runtime_api_secret_resource = plan_runtime_api_secret_resource(api, existing_links)?;
    let runtime_api_key_required = runtime_api_secret_resource
        .secrets
        .iter()
        .any(|secret| secret.action.is_some());

    Ok(PlannedRequestSigningResources {
        resources: vec![
            config_resource,
            secret_resource,
            runtime_api_secret_resource,
        ],
        bootstrap_planned: bootstrap.is_some(),
        runtime_api_key_required,
    })
}

fn determine_request_signing_bootstrap(
    config_items: &HashMap<String, String>,
    secret_names: &[String],
) -> Result<Option<RequestSigningBootstrap>, Report<CliError>> {
    let current_kid = config_items
        .get("current-kid")
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);
    let active_kids = config_items
        .get("active-kids")
        .map(|value| parse_active_kids(value))
        .unwrap_or_default();
    let has_jwk_entries = config_items
        .keys()
        .any(|key| key != "current-kid" && key != "active-kids");

    if current_kid.is_none()
        && active_kids.is_empty()
        && !has_jwk_entries
        && secret_names.is_empty()
    {
        return Ok(Some(generate_request_signing_bootstrap()?));
    }

    let Some(current_kid) = current_kid else {
        return Err(Report::new(CliError::Provisioning).attach(
            "request signing stores are partially initialized: missing `current-kid` in `jwks_store`",
        ));
    };

    if !active_kids.iter().any(|kid| kid == &current_kid) {
        return Err(Report::new(CliError::Provisioning).attach(format!(
            "request signing stores are partially initialized: `active-kids` does not include `{current_kid}`"
        )));
    }

    if !config_items.contains_key(&current_kid) {
        return Err(Report::new(CliError::Provisioning).attach(format!(
            "request signing stores are partially initialized: config store is missing JWK entry `{current_kid}`"
        )));
    }

    if !secret_names.iter().any(|name| name == &current_kid) {
        return Err(Report::new(CliError::Provisioning).attach(format!(
            "request signing stores are partially initialized: secret store is missing signing key `{current_kid}`"
        )));
    }

    Ok(None)
}

fn generate_request_signing_bootstrap() -> Result<RequestSigningBootstrap, Report<CliError>> {
    let kid = format!("ts-{}", Uuid::new_v4().simple());
    let keypair = Keypair::generate();
    let jwk_json = serde_json::to_string(&keypair.get_jwk(kid.clone()))
        .change_context(CliError::Provisioning)?;
    let private_key_base64 = general_purpose::STANDARD.encode(keypair.signing_key.to_bytes());

    Ok(RequestSigningBootstrap {
        kid,
        jwk_json,
        private_key_base64,
    })
}

fn plan_runtime_api_secret_resource(
    api: &dyn FastlyApi,
    existing_links: &[ResourceLink],
) -> Result<PlannedResource, Report<CliError>> {
    let store = api.find_secret_store_by_name(FASTLY_API_SECRET_STORE_NAME)?;
    let secret_names = match &store {
        Some(store) => api.list_secret_names(&store.id)?,
        None => Vec::new(),
    };
    let secret_exists = secret_names
        .iter()
        .any(|name| name == FASTLY_API_SECRET_KEY);
    let secret_action = (!secret_exists).then_some(ChangeKind::Create);

    Ok(PlannedResource {
        kind: ResourceKind::Secret,
        name: FASTLY_API_SECRET_STORE_NAME.to_string(),
        existing_id: store.as_ref().map(|store| store.id.clone()),
        create_store: store.is_none(),
        config_items: Vec::new(),
        secrets: vec![SecretPlan {
            name: FASTLY_API_SECRET_KEY.to_string(),
            value: SecretValuePlan::RuntimeApiKey,
            action: secret_action,
        }],
        link: Some(plan_link(
            existing_links,
            &store,
            FASTLY_API_SECRET_STORE_NAME,
        )),
    })
}

fn plan_kv_resource(
    api: &dyn FastlyApi,
    name: &str,
    existing_links: &[ResourceLink],
) -> Result<PlannedResource, Report<CliError>> {
    let store = api.find_kv_store_by_name(name)?;

    Ok(PlannedResource {
        kind: ResourceKind::Kv,
        name: name.to_string(),
        existing_id: store.as_ref().map(|store| store.id.clone()),
        create_store: store.is_none(),
        config_items: Vec::new(),
        secrets: Vec::new(),
        link: Some(plan_link(existing_links, &store, name)),
    })
}

fn parse_active_kids(active_kids: &str) -> Vec<String> {
    active_kids
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

fn plan_link(
    existing_links: &[ResourceLink],
    store: &Option<NamedResource>,
    alias: &str,
) -> LinkPlan {
    let Some(store) = store else {
        return LinkPlan {
            existing_link_id: None,
            action: Some(ChangeKind::Bind),
        };
    };

    match existing_links.iter().find(|link| link.name == alias) {
        Some(link) if link.resource_id == store.id => LinkPlan {
            existing_link_id: Some(link.id.clone()),
            action: None,
        },
        Some(link) => LinkPlan {
            existing_link_id: Some(link.id.clone()),
            action: Some(ChangeKind::Update),
        },
        None => LinkPlan {
            existing_link_id: None,
            action: Some(ChangeKind::Bind),
        },
    }
}

fn binding_changes_required(resources: &[PlannedResource]) -> bool {
    resources.iter().any(|resource| {
        resource
            .link
            .as_ref()
            .and_then(|link| link.action)
            .is_some()
    })
}

fn requires_runtime_api_key(resources: &[PlannedResource]) -> bool {
    resources.iter().any(|resource| {
        resource.secrets.iter().any(|secret| {
            secret.action.is_some() && matches!(secret.value, SecretValuePlan::RuntimeApiKey)
        })
    })
}

fn collect_actions(resources: &[PlannedResource]) -> Vec<ProvisionActionJson> {
    let mut actions = Vec::new();
    for resource in resources {
        if resource.create_store {
            actions.push(ProvisionActionJson {
                action: ChangeKind::Create,
                resource_kind: resource.kind,
                name: resource.name.clone(),
                detail: format!(
                    "create {} `{}`",
                    resource_kind_label(resource.kind),
                    resource.name
                ),
                remote_id: resource.existing_id.clone(),
            });
        }

        for item in &resource.config_items {
            if let Some(action) = item.action {
                actions.push(ProvisionActionJson {
                    action,
                    resource_kind: resource.kind,
                    name: resource.name.clone(),
                    detail: format!("set config item `{}` in `{}`", item.key, resource.name),
                    remote_id: resource.existing_id.clone(),
                });
            }
        }

        for secret in &resource.secrets {
            if let Some(action) = secret.action {
                actions.push(ProvisionActionJson {
                    action,
                    resource_kind: resource.kind,
                    name: resource.name.clone(),
                    detail: format!(
                        "upload secret `{}` to `{}` (value redacted)",
                        secret.name, resource.name
                    ),
                    remote_id: resource.existing_id.clone(),
                });
            }
        }

        if let Some(link) = &resource.link
            && let Some(action) = link.action
        {
            actions.push(ProvisionActionJson {
                action,
                resource_kind: resource.kind,
                name: resource.name.clone(),
                detail: format!(
                    "bind {} `{}` to the service",
                    resource_kind_label(resource.kind),
                    resource.name
                ),
                remote_id: resource.existing_id.clone(),
            });
        }
    }

    actions
}

fn append_request_signing_warnings(
    warnings: &mut Vec<String>,
    resources: &[PlannedResource],
    configured_config_store_id: &str,
    configured_secret_store_id: &str,
) {
    for resource in resources {
        if resource.name == JWKS_CONFIG_STORE_NAME
            && let Some(actual_id) = resource.existing_id.as_deref()
            && !configured_config_store_id.is_empty()
            && configured_config_store_id != actual_id
        {
            warnings.push(format!(
                "`request_signing.config_store_id` is `{configured_config_store_id}` but the Fastly `{}` store currently has ID `{actual_id}`; update trusted-server.toml after provisioning so runtime key rotation uses the correct ID",
                JWKS_CONFIG_STORE_NAME
            ));
        }
        if resource.name == SIGNING_SECRET_STORE_NAME
            && let Some(actual_id) = resource.existing_id.as_deref()
            && !configured_secret_store_id.is_empty()
            && configured_secret_store_id != actual_id
        {
            warnings.push(format!(
                "`request_signing.secret_store_id` is `{configured_secret_store_id}` but the Fastly `{}` store currently has ID `{actual_id}`; update trusted-server.toml after provisioning so runtime key rotation uses the correct ID",
                SIGNING_SECRET_STORE_NAME
            ));
        }
    }
}

fn create_store(
    api: &dyn FastlyApi,
    resource: &PlannedResource,
) -> Result<NamedResource, Report<CliError>> {
    match resource.kind {
        ResourceKind::Config => api.create_config_store(&resource.name),
        ResourceKind::Secret => api.create_secret_store(&resource.name),
        ResourceKind::Kv => api.create_kv_store(&resource.name),
    }
}

fn resource_kind_label(kind: ResourceKind) -> &'static str {
    match kind {
        ResourceKind::Config => "config store",
        ResourceKind::Secret => "secret store",
        ResourceKind::Kv => "KV store",
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use super::*;
    use crate::fastly::api::{FastlyApi, ServiceVersion};

    #[derive(Default)]
    struct MockFastlyApi {
        config_stores: HashMap<String, NamedResource>,
        config_items: HashMap<String, HashMap<String, String>>,
        secret_stores: HashMap<String, NamedResource>,
        secret_names: HashMap<String, Vec<String>>,
        kv_stores: HashMap<String, NamedResource>,
        versions: Vec<ServiceVersion>,
        links: Vec<ResourceLink>,
        clone_result: Option<ServiceVersion>,
        upserted_config_items: Mutex<Vec<(String, String, String)>>,
        recreated_secrets: Mutex<Vec<(String, String, String)>>,
        activated_versions: Mutex<Vec<(String, u32)>>,
    }

    impl FastlyApi for MockFastlyApi {
        fn find_config_store_by_name(
            &self,
            name: &str,
        ) -> Result<Option<NamedResource>, Report<CliError>> {
            Ok(self.config_stores.get(name).cloned())
        }

        fn create_config_store(&self, name: &str) -> Result<NamedResource, Report<CliError>> {
            Ok(NamedResource {
                id: format!("created-{name}"),
                name: name.to_string(),
            })
        }

        fn list_config_store_items(
            &self,
            store_id: &str,
        ) -> Result<HashMap<String, String>, Report<CliError>> {
            Ok(self.config_items.get(store_id).cloned().unwrap_or_default())
        }

        fn upsert_config_item(
            &self,
            store_id: &str,
            key: &str,
            value: &str,
        ) -> Result<(), Report<CliError>> {
            self.upserted_config_items
                .lock()
                .expect("should lock upserted config items")
                .push((store_id.to_string(), key.to_string(), value.to_string()));
            Ok(())
        }

        fn find_secret_store_by_name(
            &self,
            name: &str,
        ) -> Result<Option<NamedResource>, Report<CliError>> {
            Ok(self.secret_stores.get(name).cloned())
        }

        fn create_secret_store(&self, name: &str) -> Result<NamedResource, Report<CliError>> {
            Ok(NamedResource {
                id: format!("created-{name}"),
                name: name.to_string(),
            })
        }

        fn list_secret_names(&self, store_id: &str) -> Result<Vec<String>, Report<CliError>> {
            Ok(self.secret_names.get(store_id).cloned().unwrap_or_default())
        }

        fn recreate_secret(
            &self,
            store_id: &str,
            name: &str,
            value: &str,
        ) -> Result<(), Report<CliError>> {
            self.recreated_secrets
                .lock()
                .expect("should lock recreated secrets")
                .push((store_id.to_string(), name.to_string(), value.to_string()));
            Ok(())
        }

        fn find_kv_store_by_name(
            &self,
            name: &str,
        ) -> Result<Option<NamedResource>, Report<CliError>> {
            Ok(self.kv_stores.get(name).cloned())
        }

        fn create_kv_store(&self, name: &str) -> Result<NamedResource, Report<CliError>> {
            Ok(NamedResource {
                id: format!("created-{name}"),
                name: name.to_string(),
            })
        }

        fn list_service_versions(
            &self,
            _service_id: &str,
        ) -> Result<Vec<ServiceVersion>, Report<CliError>> {
            Ok(self.versions.clone())
        }

        fn clone_service_version(
            &self,
            _service_id: &str,
            version_number: u32,
        ) -> Result<ServiceVersion, Report<CliError>> {
            Ok(self.clone_result.clone().unwrap_or(ServiceVersion {
                number: version_number + 1,
                active: false,
                locked: false,
            }))
        }

        fn activate_service_version(
            &self,
            service_id: &str,
            version_number: u32,
        ) -> Result<ServiceVersion, Report<CliError>> {
            self.activated_versions
                .lock()
                .expect("should lock activated versions")
                .push((service_id.to_string(), version_number));
            Ok(ServiceVersion {
                number: version_number,
                active: true,
                locked: true,
            })
        }

        fn list_resource_links(
            &self,
            _service_id: &str,
            _version_number: u32,
        ) -> Result<Vec<ResourceLink>, Report<CliError>> {
            Ok(self.links.clone())
        }

        fn create_resource_link(
            &self,
            _service_id: &str,
            _version_number: u32,
            resource_id: &str,
            name: &str,
        ) -> Result<ResourceLink, Report<CliError>> {
            Ok(ResourceLink {
                id: format!("link-{name}"),
                name: name.to_string(),
                resource_id: resource_id.to_string(),
            })
        }

        fn update_resource_link(
            &self,
            _service_id: &str,
            _version_number: u32,
            link_id: &str,
            resource_id: &str,
            name: &str,
        ) -> Result<ResourceLink, Report<CliError>> {
            Ok(ResourceLink {
                id: link_id.to_string(),
                name: name.to_string(),
                resource_id: resource_id.to_string(),
            })
        }
    }

    fn validated_config(enable_request_signing: bool) -> crate::config::ValidatedConfig {
        let tempdir = tempfile::tempdir().expect("should create tempdir");
        let path = tempdir.path().join("trusted-server.toml");
        let mut config = crate::config::STARTER_CONFIG_TEMPLATE.to_string();
        if enable_request_signing {
            config = config.replace(
                "enabled = false  # Set to true to enable request signing",
                "enabled = true",
            );
        }
        std::fs::write(&path, config).expect("should write config");
        crate::config::load_validated_config(Some(&path)).expect("should validate config")
    }

    #[test]
    fn plan_reports_create_update_and_bind_actions() {
        let config_store = NamedResource {
            id: "cfg_123".to_string(),
            name: APPLICATION_CONFIG_STORE_NAME.to_string(),
        };
        let api = MockFastlyApi {
            config_stores: HashMap::from([(
                APPLICATION_CONFIG_STORE_NAME.to_string(),
                config_store.clone(),
            )]),
            config_items: HashMap::from([(
                config_store.id.clone(),
                HashMap::from([(APPLICATION_CONFIG_KEY.to_string(), "old".to_string())]),
            )]),
            versions: vec![ServiceVersion {
                number: 9,
                active: true,
                locked: true,
            }],
            ..Default::default()
        };
        let validated = validated_config(false);

        let plan = plan_fastly_provisioning(&api, &validated, "svc_123")
            .expect("should plan provisioning");

        assert!(
            plan.json
                .actions
                .iter()
                .any(|action| action.action == ChangeKind::Update
                    && action.name == APPLICATION_CONFIG_STORE_NAME),
            "should plan runtime config update"
        );
        assert!(
            plan.json.service_version.clone_required,
            "should require a clone when bindings would be added on a locked version"
        );
    }

    #[test]
    fn plan_bootstraps_empty_request_signing_stores_and_warns_about_runtime_token() {
        let api = MockFastlyApi {
            versions: vec![ServiceVersion {
                number: 9,
                active: true,
                locked: false,
            }],
            ..Default::default()
        };
        let validated = validated_config(true);

        let plan = plan_fastly_provisioning(&api, &validated, "svc_123")
            .expect("should plan provisioning");

        assert!(
            plan.json
                .actions
                .iter()
                .any(|action| action.detail.contains("set config item `current-kid`")),
            "should seed current-kid"
        );
        assert!(
            plan.json
                .actions
                .iter()
                .any(|action| action.detail.contains("set config item `active-kids`")),
            "should seed active-kids"
        );
        assert!(
            plan.json
                .actions
                .iter()
                .any(|action| action.name == SIGNING_SECRET_STORE_NAME
                    && action.detail.contains("upload secret `ts-")),
            "should upload an initial signing secret"
        );
        assert!(
            plan.json
                .warnings
                .iter()
                .any(|warning| warning.contains("uninitialized")),
            "should warn about signing key bootstrap"
        );
        assert!(
            plan.json
                .warnings
                .iter()
                .any(|warning| warning.contains("FASTLY_RUNTIME_API_KEY")),
            "should warn that apply needs an explicit runtime token"
        );
    }

    #[test]
    fn apply_requires_explicit_runtime_token_when_request_signing_needs_one() {
        let api = MockFastlyApi {
            versions: vec![ServiceVersion {
                number: 9,
                active: true,
                locked: false,
            }],
            ..Default::default()
        };
        let validated = validated_config(true);

        let error = apply_fastly_provisioning(&api, &validated, "svc_123", None, true)
            .expect_err("should reject implicit reuse of the management token");

        assert!(
            format!("{error:?}").contains("FASTLY_RUNTIME_API_KEY"),
            "should explain how to provide the runtime token"
        );
    }

    #[test]
    fn apply_activates_target_version_when_bindings_change() {
        let api = MockFastlyApi {
            versions: vec![ServiceVersion {
                number: 9,
                active: true,
                locked: true,
            }],
            clone_result: Some(ServiceVersion {
                number: 10,
                active: false,
                locked: false,
            }),
            ..Default::default()
        };
        let validated = validated_config(false);

        let applied = apply_fastly_provisioning(&api, &validated, "svc_123", None, true)
            .expect("should apply provisioning");

        assert!(
            applied.activated_version,
            "should activate the modified version"
        );
        assert_eq!(
            api.activated_versions
                .lock()
                .expect("should lock activated versions")
                .as_slice(),
            &[("svc_123".to_string(), 10)],
            "should activate the cloned target version"
        );
    }
}
