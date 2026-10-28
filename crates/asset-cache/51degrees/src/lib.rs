//! An asset cache module that holds the files the publisher's origin sends
//! that are not pages, and the answers of the asset routes, as each origin
//! sent them.
//!
//! `[asset-cache] module = "51degrees"` selects it, and core then offers it
//! each `GET` for such a file before asking the file's origin, see
//! [`trusted_server_core::asset_cache`]. A file's copies are held in
//! `51degrees-assets` under what the origin's `Vary` names, which the module
//! records for each address in `51degrees-assets-vary`. Both are loading
//! caches the module declares, so each takes a loading cache module the way
//! every declared cache does, and holds nothing in a build that links none.

pub(crate) mod entry;
mod hash;
mod load;
pub(crate) mod source_cache;

use std::sync::Arc;

use error_stack::Report;
use trusted_server_core::asset_cache::{AssetAnswer, AssetCacheModule, AssetFile, AssetVia};
use trusted_server_core::error::TrustedServerError;
use trusted_server_core::integrations::{IntegrationBuilder, IntegrationRegistration};
use trusted_server_core::platform::{CacheLifetimes, CacheSpec, RuntimeServices};
use trusted_server_core::settings::{LoadingCacheConfig, Settings};

use crate::entry::AssetCacheKey;
use crate::source_cache::SourceCache;

/// The name of this module, from this crate's folder.
pub const MODULE: &str = "asset-cache.51degrees";

/// The cache that holds the copies, whose lifetimes `[cache.51degrees-assets]`
/// sets.
pub const ASSETS_CACHE: &str = "51degrees-assets";

/// The cache that holds each address's record of what its responses vary on,
/// with the lifetimes `[cache.51degrees-assets]` sets for the copies.
pub const ASSETS_VARY_CACHE: &str = "51degrees-assets-vary";

const INTEGRATION_ID: &str = "asset_cache_51degrees";

/// The lifetimes `[cache.51degrees-assets]` leaves unset. A file's own
/// lifetime ends each copy sooner than the store lifetime.
const ASSET_DEFAULT_LIFETIMES: CacheLifetimes = CacheLifetimes {
    memory: std::time::Duration::from_secs(5),
    store: std::time::Duration::from_secs(60 * 60),
    store_idle: std::time::Duration::from_secs(24 * 60 * 60),
};

/// The most records the memory layer of `51degrees-assets-vary` holds.
const MEMORY_RECORDS: usize = 256;

/// The most copies the memory layer of `51degrees-assets` holds.
const MEMORY_COPIES: usize = 32;

/// The builder a deployment hands to an adapter, which the registry runs
/// when `[asset-cache]` selects [`MODULE`].
#[must_use]
pub fn builder() -> IntegrationBuilder {
    IntegrationBuilder::new(INTEGRATION_ID, env!("CARGO_PKG_NAME"), register, validate)
        .with_module_name(MODULE)
        .with_cache_features(&[ASSETS_CACHE, ASSETS_VARY_CACHE])
}

/// Reports whether a section selects the module.
///
/// The module reads no table of its own, so it refuses an
/// `[asset-cache.51degrees]` table, an empty one included. It refuses a
/// lifetime in `[cache.51degrees-assets-vary]` whether or not it is selected.
fn validate(settings: &Settings) -> Result<bool, Report<TrustedServerError>> {
    refuse_record_lifetimes(settings)?;
    let Some((section, written)) = settings.module_selection(MODULE) else {
        return Ok(false);
    };
    let has_table = settings
        .module_sections()
        .any(|(name, modules)| name == section && modules.settings_of(written).is_some());
    if has_table {
        return Err(Report::new(TrustedServerError::Configuration {
            message: format!(
                "[{section}.{written}] is a table the module does not read, because it \
                 takes no setting of its own, so remove the table. The lifetimes of its \
                 caches are set in [cache.{ASSETS_CACHE}]"
            ),
        }));
    }
    Ok(true)
}

/// Refuses each lifetime `[cache.51degrees-assets-vary]` sets, because the
/// records take the lifetimes of `[cache.51degrees-assets]` and nothing reads
/// the table's own.
fn refuse_record_lifetimes(settings: &Settings) -> Result<(), Report<TrustedServerError>> {
    let Some(table) = settings.cache.declared.get(ASSETS_VARY_CACHE) else {
        return Ok(());
    };
    // Every field is named, so a setting added to the table does not build
    // here until this check reads or refuses it.
    let LoadingCacheConfig {
        module: _,
        memory_seconds,
        store_seconds,
        store_idle_seconds,
    } = table;
    let set: Vec<String> = [
        ("memory_seconds", memory_seconds),
        ("store_seconds", store_seconds),
        ("store_idle_seconds", store_idle_seconds),
    ]
    .into_iter()
    .filter(|(_, value)| value.is_some())
    .map(|(name, _)| format!("`{name}`"))
    .collect();
    let named = match set.split_last() {
        None => return Ok(()),
        Some((last, [])) => last.clone(),
        Some((last, rest)) => format!("{} and {last}", rest.join(", ")),
    };
    Err(Report::new(TrustedServerError::Configuration {
        message: format!(
            "[cache.{ASSETS_VARY_CACHE}] sets {named}, which {MODULE} does not read, because \
             its records take the lifetimes of [cache.{ASSETS_CACHE}]. Set the lifetimes \
             there, as this table takes only `module`"
        ),
    }))
}

/// Supplies the module when `[asset-cache]` selects it.
fn register(
    settings: &Settings,
) -> Result<Option<IntegrationRegistration>, Report<TrustedServerError>> {
    if !validate(settings)? {
        return Ok(None);
    }
    Ok(Some(
        IntegrationRegistration::builder(INTEGRATION_ID)
            .without_js()
            .with_asset_cache_module(MODULE, Arc::new(AssetCache::from_settings(settings)))
            .build(),
    ))
}

/// The module, which answers for a file from the copies it holds and loads
/// the file from its origin when none serves.
#[derive(Debug)]
pub struct AssetCache {
    caches: SourceCache,
}

impl AssetCache {
    /// The module with both of its caches given the lifetimes
    /// `[cache.51degrees-assets]` sets, and the defaults for any it leaves
    /// unset.
    #[must_use]
    pub fn from_settings(settings: &Settings) -> Self {
        let lifetimes = settings
            .cache
            .declared
            .get(ASSETS_CACHE)
            .map_or(ASSET_DEFAULT_LIFETIMES, |config| {
                config.lifetimes(ASSET_DEFAULT_LIFETIMES)
            });
        let spec = |name, memory_entries| CacheSpec {
            name,
            lifetimes,
            memory_entries,
        };
        Self {
            caches: SourceCache {
                records: spec(ASSETS_VARY_CACHE, MEMORY_RECORDS),
                copies: spec(ASSETS_CACHE, MEMORY_COPIES),
            },
        }
    }
}

#[async_trait::async_trait(?Send)]
impl AssetCacheModule for AssetCache {
    async fn answer(&self, services: &RuntimeServices, file: AssetFile<'_>) -> AssetAnswer {
        if !may_use_a_copy(services, &file) {
            return AssetAnswer::AskOrigin;
        }
        let key = AssetCacheKey {
            url: file.origin_url.to_owned(),
            request_host: file.host.to_owned(),
            request_scheme: file.scheme.to_owned(),
            origin_identity: file.origin_identity.to_owned(),
        };
        match load::asset_from_cache(services, &self.caches, &key, file.origin).await {
            Some(Ok(response)) => AssetAnswer::Serve(response),
            Some(Err(report)) => AssetAnswer::OriginFailed(report),
            None => AssetAnswer::AskOrigin,
        }
    }
}

/// Whether a copy may answer for `file`, which core offers only as a `GET`.
///
/// On the publisher path the origin receives the reader's cookies, and
/// nothing shows whether they shaped its answer, so a request carrying a
/// cookie is the origin's, as a document is. A route sends its origin no
/// cookie. A record cache that keeps nothing would run the record's load,
/// and so fetch the origin, for every request, so both caches must keep
/// values.
fn may_use_a_copy(services: &RuntimeServices, file: &AssetFile<'_>) -> bool {
    let reader_allows = match file.via {
        AssetVia::Publisher => !file.is_document && !file.carries_cookie,
        AssetVia::Route => true,
    };
    reader_allows
        && !file.host.is_empty()
        && !file.carries_authorization
        && !file.asks_for_origin
        && [ASSETS_CACHE, ASSETS_VARY_CACHE]
            .iter()
            .all(|name| services.loading_cache_for(name).keeps_values())
}

#[cfg(test)]
mod tests;
