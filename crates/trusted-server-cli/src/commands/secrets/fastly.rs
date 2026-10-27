use std::fs;
use std::path::Path;

use edgezero_cli::fastly_secrets::{
    self, FASTLY_SECRET_MAX_BYTES, FastlySecretResources, SecretWriteAction,
};
use edgezero_core::canonical_form::canonical_data_sha256;
use edgezero_core::manifest::ManifestLoader;
use serde_json::{Value, json};

use super::{Adapter, NativeBinding, Provider, SECRET_STORE_ID, TargetArgs};

pub(super) struct NativeProvider<'a> {
    pub(super) transport: Option<&'a dyn fastly_secrets::FastlySecretTransport>,
}

impl Provider for NativeProvider<'_> {
    fn resources(&self, target: &TargetArgs) -> Result<Value, String> {
        if target.adapter == Adapter::Fastly {
            let context = fastly_secrets::context_from_manifest(
                &target.config.manifest,
                target.service_id.as_deref(),
            )?;
            let resources = match self.transport {
                Some(transport) => {
                    fastly_secrets::resolve_resources_with_transport(&context, transport)?
                }
                None => fastly_secrets::resolve_resources(&context)?,
            };
            ensure_secret_store(&resources)?;
            return serde_json::to_value(resources)
                .map_err(|_| "could not encode resolved deployment resources".to_owned());
        }
        native_resources(target)
    }

    fn exists(&self, resources: &Value, key: &str) -> Result<bool, String> {
        let resources = fastly_resources(resources)?;
        let resource_id = &ensure_secret_store(&resources)?.resource_id;
        match self.transport {
            Some(transport) => {
                fastly_secrets::secret_exists_with_transport(resource_id, key, transport)
            }
            None => fastly_secrets::secret_exists(resource_id, key),
        }
    }

    fn write(
        &self,
        resources: &Value,
        key: &str,
        value: &[u8],
        replace: bool,
    ) -> Result<(), String> {
        let resources = fastly_resources(resources)?;
        let resource_id = &ensure_secret_store(&resources)?.resource_id;
        let action = if replace {
            SecretWriteAction::Replace
        } else {
            SecretWriteAction::Create
        };
        match self.transport {
            Some(transport) => fastly_secrets::write_secret_with_transport(
                resource_id,
                key,
                value,
                action,
                transport,
            ),
            None => fastly_secrets::write_secret(resource_id, key, value, action),
        }
    }

    fn max_secret_bytes(&self) -> usize {
        FASTLY_SECRET_MAX_BYTES
    }
}

fn fastly_resources(resources: &Value) -> Result<FastlySecretResources, String> {
    // Only freshly resolved in-process metadata reaches this function. Reviewed
    // plan contents are compared, never deserialized into write authority.
    serde_json::from_value(resources.clone())
        .map_err(|_| "could not read resolved deployment resources".to_owned())
}

fn ensure_secret_store(
    resources: &FastlySecretResources,
) -> Result<&fastly_secrets::FastlyStoreResource, String> {
    resources
        .stores
        .iter()
        .find(|store| store.kind == "secrets" && store.logical_id == SECRET_STORE_ID)
        .ok_or_else(|| "deployment does not declare trusted_server_secrets".to_owned())
}

pub(super) fn spin_component(target: &TargetArgs) -> Result<Option<String>, String> {
    if target.adapter != Adapter::Spin {
        return Ok(None);
    }
    let loader = ManifestLoader::from_path(&target.config.manifest)
        .map_err(|_| "could not load deployment manifest".to_owned())?;
    let (_, adapter) = loader
        .manifest()
        .adapter_entry("spin")
        .ok_or_else(|| "Spin is not declared in the deployment manifest".to_owned())?;
    let path = loader
        .manifest()
        .root()
        .unwrap_or_else(|| Path::new("."))
        .join(adapter.adapter.manifest.as_deref().ok_or_else(|| {
            "Spin secret planning requires an explicit adapter manifest".to_owned()
        })?);
    edgezero_cli::resolve_spin_component(&path, adapter.adapter.component.as_deref())
        .map(Some)
        .map_err(|_| {
            "could not resolve selected Spin component from the existing manifests".to_owned()
        })
}

pub(super) fn validate_native_declarations(
    target: &TargetArgs,
    bindings: &[&NativeBinding],
) -> Result<(), String> {
    let loader = ManifestLoader::from_path(&target.config.manifest)
        .map_err(|_| "could not load deployment manifest".to_owned())?;
    let manifest = loader.manifest();
    let environment = manifest.environment_for(target.adapter.name());
    for binding in bindings {
        if environment
            .variables
            .iter()
            .chain(&environment.secrets)
            .any(|declaration| declaration.env == binding.native_name)
        {
            return Err(
                "generated native identity collides with another environment declaration"
                    .to_owned(),
            );
        }
    }
    let (_, adapter) = manifest
        .adapter_entry(target.adapter.name())
        .ok_or_else(|| "adapter is not declared in the deployment manifest".to_owned())?;
    let Some(path) = &adapter.adapter.manifest else {
        return Ok(());
    };
    let path = manifest.root().unwrap_or_else(|| Path::new(".")).join(path);
    let text =
        fs::read_to_string(path).map_err(|_| "could not read adapter manifest".to_owned())?;
    let document: toml::Value =
        toml::from_str(&text).map_err(|_| "could not parse adapter manifest".to_owned())?;
    let selected_component = spin_component(target)?;
    for binding in bindings {
        if target.adapter == Adapter::Cloudflare
            && worker_collision(&document, &binding.native_name)
        {
            return Err("generated native secret collides with another Worker binding or plaintext variable".to_owned());
        }
        if target.adapter == Adapter::Spin {
            if let Some(variable) = document
                .get("variables")
                .and_then(|vars| vars.get(&binding.native_name))
                && (variable.get("secret").and_then(toml::Value::as_bool) != Some(true)
                    || variable.get("required").and_then(toml::Value::as_bool) != Some(true)
                    || variable.get("default").is_some())
            {
                return Err(
                    "generated Spin identity collides with an incompatible application variable"
                        .to_owned(),
                );
            }
            if let Some(variable) = document
                .get("component")
                .and_then(|components| {
                    components.get(selected_component.as_deref().unwrap_or("trusted-server"))
                })
                .and_then(|component| component.get("variables"))
                .and_then(|vars| vars.get(&binding.native_name))
                && variable.as_str() != Some(format!("{{{{ {} }}}}", binding.native_name).as_str())
            {
                return Err(
                    "generated Spin identity collides with an incompatible component variable"
                        .to_owned(),
                );
            }
        }
    }
    Ok(())
}

fn worker_collision(value: &toml::Value, name: &str) -> bool {
    match value {
        toml::Value::Table(table) => {
            if table.get("binding").and_then(toml::Value::as_str) == Some(name) {
                return true;
            }
            table.iter().any(|(key, value)| {
                let named_map = matches!(
                    key.as_str(),
                    "vars" | "wasm_modules" | "text_blobs" | "data_blobs"
                ) && value.get(name).is_some();
                let named_binding = key == "bindings"
                    && value.as_array().is_some_and(|items| {
                        items.iter().any(|item| {
                            item.get("name").and_then(toml::Value::as_str) == Some(name)
                        })
                    });
                named_map || named_binding || worker_collision(value, name)
            })
        }
        toml::Value::Array(items) => items.iter().any(|item| worker_collision(item, name)),
        _ => false,
    }
}

pub(super) fn runtime_secret_store(target: &TargetArgs) -> Result<String, String> {
    if !matches!(target.adapter, Adapter::Axum | Adapter::Spin) {
        return Ok(SECRET_STORE_ID.to_owned());
    }
    let loader = ManifestLoader::from_path(&target.config.manifest)
        .map_err(|_| "could not load deployment manifest".to_owned())?;
    edgezero_cli::effective_manifest_environment(
        loader.manifest(),
        target.adapter.name(),
        std::env::vars(),
    )
    .store_name_checked("secrets", SECRET_STORE_ID)
    .map_err(|_| "invalid trusted runtime secret-store selector".to_owned())
}

fn native_resources(target: &TargetArgs) -> Result<Value, String> {
    let loader = ManifestLoader::from_path(&target.config.manifest)
        .map_err(|_| "could not load deployment manifest".to_owned())?;
    let manifest = loader.manifest();
    let (_, adapter) = manifest
        .adapter_entry(target.adapter.name())
        .ok_or_else(|| "adapter is not declared in the deployment manifest".to_owned())?;
    let secrets = manifest
        .stores
        .secrets
        .as_ref()
        .ok_or_else(|| "deployment does not declare trusted_server_secrets".to_owned())?;
    if !secrets.ids.iter().any(|id| id == SECRET_STORE_ID) {
        return Err("deployment does not declare trusted_server_secrets".to_owned());
    }
    let runtime_store = runtime_secret_store(target)?;
    let path = adapter
        .adapter
        .manifest
        .as_ref()
        .map(|path| manifest.root().unwrap_or_else(|| Path::new(".")).join(path));
    let adapter_manifest = path
        .as_ref()
        .map(|path| {
            let text = fs::read_to_string(path)
                .map_err(|_| "could not read adapter manifest".to_owned())?;
            let value: toml::Value =
                toml::from_str(&text).map_err(|_| "could not parse adapter manifest".to_owned())?;
            serde_json::to_value(value).map_err(|_| "could not encode adapter manifest".to_owned())
        })
        .transpose()?
        .unwrap_or(Value::Null);
    let component = spin_component(target)?;
    Ok(json!({
        "logical_store": SECRET_STORE_ID,
        "runtime_secret_store": runtime_store,
        "resource_resolution": "native provider resource IDs are not resolved by this adapter",
        "adapter_manifest_sha256": canonical_data_sha256(&adapter_manifest),
        "component": component,
        "application": adapter_manifest.get("name").or_else(|| adapter_manifest.pointer("/application/name")),
    }))
}
