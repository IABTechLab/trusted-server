//! How the module answers for a file from its caches, and loads the file
//! from its origin when no copy serves.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::{Rc, Weak};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

use bytes::Bytes;
use edgezero_core::body::Body as EdgeBody;
use error_stack::Report;
use futures::StreamExt as _;
use http::{Response, StatusCode, header};
use trusted_server_core::asset_cache::OriginRequest;
use trusted_server_core::platform::{LoadError, PlatformError, RuntimeServices};
use trusted_server_core::publisher::{KeepRefusal, origin_response_may_be_kept, origin_shared_ttl};
use web_time::Instant;

use crate::entry::{AssetCacheKey, AssetCopy, AssetEntry, MAX_ASSET_COPY_BYTES};
use crate::source_cache::{
    Fetch, Fetched, Found, Lookup, SourceAddress, SourceCache, SourceVary, get_or_fetch,
};

/// How long a file the origin says may not be shared is held as such, so
/// the requests that follow fetch their own rather than waiting on a load.
const ASSET_NOT_SHAREABLE_LIFETIME: Duration = Duration::from_secs(60);

/// The address one file's copies are held under, made of the publisher's
/// prefix the request's services carry, so two publishers never share one,
/// and the file's own key.
fn asset_address(services: &RuntimeServices, key: &AssetCacheKey) -> SourceAddress {
    SourceAddress::new(services.server_key_prefix().clone(), key.to_cache_key())
}

/// The answer for the file `key` names, or `None` when core is to ask the
/// origin. The answer is a kept copy, the response this request's own load
/// fetched and may not share, or the failure of the load this request ran or
/// waited on, which core treats as its own origin request failing, so the
/// origin is not asked again.
pub(crate) async fn asset_from_cache(
    services: &RuntimeServices,
    cache: &SourceCache,
    key: &AssetCacheKey,
    origin: OriginRequest,
) -> Option<Result<Response<EdgeBody>, Report<PlatformError>>> {
    let address = asset_address(services, key);
    let hand_back = HandBack::new();
    let load = Arc::new(AssetLoad {
        services: services.clone().without_connection(),
        origin,
        max_lifetime: cache.copies.lifetimes.store,
    });
    let fetch = asset_fetch(Arc::clone(&load), hand_back.id);
    let not_shareable = Bytes::from(AssetEntry::NotShareable.encode());
    let found = get_or_fetch(
        &Lookup {
            services,
            cache,
            address: &address,
            headers: load.origin.headers(),
            declared: None,
            not_shareable: &not_shareable,
        },
        fetch,
    )
    .await;
    match found {
        Ok(Found::Copy { value, key }) => match AssetEntry::decode(&value) {
            Ok(AssetEntry::Copy(copy)) => Some(Ok(copy.into_response(unix_seconds()))),
            // This request's own load fetched the file, so it is served from
            // that fetch rather than a second one.
            Ok(AssetEntry::NotShareable) => hand_back.take().map(Ok),
            Err(miss) => {
                log::warn!("asset_cache invalid entry: {miss}; removing it");
                services
                    .loading_cache_for(cache.copies.name)
                    .remove(services, &cache.copies, &key)
                    .await;
                None
            }
        },
        Ok(Found::NoCopies) => hand_back.take().map(Ok),
        // The load failed asking the origin, and every request waiting on it
        // is given that failure, so none of them asks the origin again.
        Err(error) => {
            log::warn!("asset_cache load failed: {error}");
            Some(Err(
                Report::new(error).change_context(PlatformError::HttpClient)
            ))
        }
    }
}

/// Why a file's response is not kept.
#[derive(Debug, derive_more::Display)]
enum AssetCacheBypassReason {
    /// A page is kept as a template, by the template cache, or not at all.
    #[display("the response is HTML, which is not kept as an asset")]
    Html,
    /// The body is larger than a copy holds.
    #[display("the response is larger than a kept asset holds")]
    TooLarge,
    /// The body outgrew a copy, or failed, while it was read.
    #[display("the response could not be read whole")]
    NotReadWhole,
    /// A header cannot be written down to be served again.
    #[display("a response header cannot be kept")]
    UnkeptHeader,
    /// The response varies on every request header.
    #[display("origin response varies on *")]
    VaryWildcard,
    /// The response varies on the reader's cookies.
    #[display("origin response varies on Cookie")]
    VaryCookie,
    /// The response's `Vary` cannot be read.
    #[display("origin response Vary cannot be read")]
    VaryUnreadable,
    /// The origin's status, cookies or cache policy do not allow a shared
    /// copy, or the policy cannot be read.
    #[display("{_0}")]
    Policy(KeepRefusal),
}

/// Why a response that varies this way is served to no other request.
fn vary_bypass_reason(vary: &SourceVary) -> Option<AssetCacheBypassReason> {
    match vary {
        SourceVary::Headers(_) => None,
        SourceVary::Any => Some(AssetCacheBypassReason::VaryWildcard),
        SourceVary::Cookie => Some(AssetCacheBypassReason::VaryCookie),
        SourceVary::Unreadable => Some(AssetCacheBypassReason::VaryUnreadable),
    }
}

/// How long a file's response may be kept, from its own cache policy and
/// capped at `max_lifetime`, or why it is not kept at all.
fn asset_cache_ttl(
    status: StatusCode,
    content_type: &str,
    headers: &http::HeaderMap,
    max_lifetime: Duration,
) -> Result<Duration, AssetCacheBypassReason> {
    origin_response_may_be_kept(status, headers).map_err(AssetCacheBypassReason::Policy)?;
    if is_html_content_type(content_type) {
        return Err(AssetCacheBypassReason::Html);
    }
    origin_shared_ttl(headers, max_lifetime).map_err(AssetCacheBypassReason::Policy)
}

/// Seconds since the Unix epoch, from the wasm-safe clock.
fn unix_seconds() -> u64 {
    std_system_time_now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

/// The current time as a [`std::time::SystemTime`] without calling
/// `std::time::SystemTime::now()`, which panics on `wasm32-unknown-unknown`.
/// It reads the [`web_time`] clock, which on native and `wasm32-wasip1` is the
/// std clock.
fn std_system_time_now() -> SystemTime {
    std::time::UNIX_EPOCH
        + web_time::SystemTime::now()
            .duration_since(web_time::UNIX_EPOCH)
            .unwrap_or_default()
}

fn is_html_content_type(content_type: &str) -> bool {
    content_type_contains_ascii_case_insensitive(content_type, "text/html")
}

fn content_type_contains_ascii_case_insensitive(content_type: &str, needle: &str) -> bool {
    content_type.to_ascii_lowercase().contains(needle)
}

fn single_header_value(
    headers: &http::HeaderMap,
    name: header::HeaderName,
) -> Result<Option<&str>, KeepRefusal> {
    let mut values = headers.get_all(name).iter();
    let Some(first) = values.next() else {
        return Ok(None);
    };
    if values.next().is_some() {
        return Err(KeepRefusal::MalformedPolicy);
    }
    first
        .to_str()
        .map(Some)
        .map_err(|_| KeepRefusal::MalformedPolicy)
}

fn parse_delta_seconds(value: &str) -> Result<u64, KeepRefusal> {
    let value = value.trim();
    let quoted_at_start = value.starts_with('"');
    let quoted_at_end = value.ends_with('"');
    let digits = match (quoted_at_start, quoted_at_end) {
        (true, true) if value.len() >= 2 => &value[1..value.len() - 1],
        (false, false) => value,
        _ => return Err(KeepRefusal::MalformedPolicy),
    };
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(KeepRefusal::MalformedPolicy);
    }
    digits
        .parse::<u64>()
        .map_err(|_| KeepRefusal::MalformedPolicy)
}

type HandBackSlot = RefCell<Option<Response<EdgeBody>>>;

/// The slot of each load on this thread whose request is still waiting, by
/// the load's id.
type HandBackSlots = RefCell<HashMap<u64, Weak<HandBackSlot>>>;

thread_local! {
    static HANDED_BACK: HandBackSlots = RefCell::new(HashMap::new());
}

static NEXT_LOAD: AtomicU64 = AtomicU64::new(0);

/// Where a load leaves a response the request that started it is served,
/// because the origin answered with something no other request may be
/// served, so that request is served from the one fetch rather than a
/// second one. The slot lives as long as the request's `HandBack`, on the
/// thread that runs the request.
struct HandBack {
    id: u64,
    slot: Rc<HandBackSlot>,
}

impl HandBack {
    fn new() -> Self {
        let id = NEXT_LOAD.fetch_add(1, Ordering::Relaxed);
        let slot = Rc::new(RefCell::new(None));
        HANDED_BACK.with(|handed| {
            handed.borrow_mut().insert(id, Rc::downgrade(&slot));
        });
        Self { id, slot }
    }

    fn take(&self) -> Option<Response<EdgeBody>> {
        self.slot.borrow_mut().take()
    }
}

impl Drop for HandBack {
    fn drop(&mut self) {
        HANDED_BACK.with(|handed| {
            handed.borrow_mut().remove(&self.id);
        });
    }
}

fn hand_back(id: u64, response: Response<EdgeBody>) {
    let slot = HANDED_BACK.with(|handed| handed.borrow().get(&id).and_then(Weak::upgrade));
    if let Some(slot) = slot {
        *slot.borrow_mut() = Some(response);
    }
}

/// What a load owns to fetch one file, because a loader owns everything it
/// uses.
struct AssetLoad {
    /// The request's services without its connection, because one load
    /// serves every request waiting on it.
    services: RuntimeServices,
    /// The request core would send to the origin.
    origin: OriginRequest,
    /// The longest a copy may be kept, whatever the origin asks.
    max_lifetime: Duration,
}

/// What one fetch of a file made.
struct AssetLoaded {
    /// What the origin's response varies on.
    vary: SourceVary,
    /// The copy with the time it may be used for, or `None` when the response
    /// may not be shared.
    copy: Option<(AssetCopy, Duration)>,
}

/// The module's fetch for one request, which fetches the file and, when it
/// may be shared, keeps it as the origin sent it.
///
/// A file that may not be shared is held as [`AssetEntry::NotShareable`] for
/// [`ASSET_NOT_SHAREABLE_LIFETIME`], and handed back to the request whose
/// load fetched it through `hand_back_id`, see [`HandBack`].
fn asset_fetch(load: Arc<AssetLoad>, hand_back_id: u64) -> Fetch {
    Arc::new(move || {
        let load = Arc::clone(&load);
        Box::pin(async move {
            let AssetLoaded { vary, copy } = load_asset(&load, hand_back_id).await?;
            let (entry, lifetime) = match copy {
                Some((copy, lifetime)) => (AssetEntry::Copy(copy), lifetime),
                None => (AssetEntry::NotShareable, ASSET_NOT_SHAREABLE_LIFETIME),
            };
            Ok(Fetched {
                vary,
                copy: entry.encode().into(),
                lifetime,
            })
        })
    })
}

/// `body` read whole when it holds at most `limit` bytes, or else the body to
/// serve in its place, being what was read so far and then the rest.
async fn read_whole_or_hand_back(body: EdgeBody, limit: usize) -> Result<Bytes, EdgeBody> {
    let mut stream = match body {
        EdgeBody::Once(bytes) if bytes.len() > limit => return Err(EdgeBody::Once(bytes)),
        EdgeBody::Once(bytes) => return Ok(bytes),
        EdgeBody::Stream(stream) => stream,
    };
    let mut read = Vec::new();
    while let Some(chunk) = stream.next().await {
        let failed = match chunk {
            Ok(chunk) => {
                read.extend_from_slice(&chunk);
                if read.len() <= limit {
                    continue;
                }
                None
            }
            Err(error) => Some(Err(error)),
        };
        let head = std::iter::once(Ok(Bytes::from(read))).chain(failed);
        return Err(EdgeBody::Stream(
            futures::stream::iter(head).chain(stream).boxed_local(),
        ));
    }
    Ok(read.into())
}

/// Fetches a file and keeps it as the origin sent it, with the time it may
/// be used for, or no copy when it may not be shared.
///
/// # Errors
///
/// When the origin cannot be reached.
async fn load_asset(load: &AssetLoad, hand_back_id: u64) -> Result<AssetLoaded, LoadError> {
    let response = load
        .services
        .http_client()
        .send(load.origin.platform_request())
        .await
        .map_err(|report| LoadError::new(format!("the origin could not be reached: {report:?}")))?
        .response;
    let made = Instant::now();
    let (parts, body) = response.into_parts();
    let content_type = parts
        .headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let vary = SourceVary::of(&parts.headers);
    let not_shareable = |vary, parts, body| {
        hand_back(hand_back_id, Response::from_parts(parts, body));
        Ok(AssetLoaded { vary, copy: None })
    };
    if let Some(reason) = vary_bypass_reason(&vary) {
        log::debug!("asset_cache bypass: {reason}");
        return not_shareable(vary, parts, body);
    }
    let ttl = match asset_cache_ttl(
        parts.status,
        &content_type,
        &parts.headers,
        load.max_lifetime,
    ) {
        Ok(ttl) => ttl,
        Err(reason) => {
            log::debug!("asset_cache bypass: {reason}");
            return not_shareable(vary, parts, body);
        }
    };
    let declared_length = parts
        .headers
        .get(header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse::<u64>().ok());
    if declared_length.is_some_and(|length| length > MAX_ASSET_COPY_BYTES as u64) {
        log::debug!("asset_cache bypass: {}", AssetCacheBypassReason::TooLarge);
        return not_shareable(vary, parts, body);
    }
    let Some(headers) = AssetCopy::kept_headers(&parts.headers, false) else {
        log::debug!(
            "asset_cache bypass: {}",
            AssetCacheBypassReason::UnkeptHeader
        );
        return not_shareable(vary, parts, body);
    };
    let age_at_made = single_header_value(&parts.headers, header::AGE)
        .ok()
        .flatten()
        .and_then(|value| parse_delta_seconds(value).ok())
        .unwrap_or(0);
    let raw = match read_whole_or_hand_back(body, MAX_ASSET_COPY_BYTES).await {
        Ok(raw) => raw,
        Err(body) => {
            log::debug!(
                "asset_cache bypass: {}",
                AssetCacheBypassReason::NotReadWhole
            );
            return not_shareable(vary, parts, body);
        }
    };
    let lifetime = ttl.saturating_sub(made.elapsed());
    if !lifetime.is_zero() {
        log::debug!("asset_cache stored {} bytes", raw.len());
    }
    Ok(AssetLoaded {
        vary,
        copy: Some((
            AssetCopy {
                headers,
                decoded: false,
                made_at: unix_seconds(),
                age_at_made,
                body: raw.to_vec(),
            },
            lifetime,
        )),
    })
}
