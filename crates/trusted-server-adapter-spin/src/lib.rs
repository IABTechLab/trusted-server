//! Fermyon Spin adapter for Trusted Server.

pub mod app;
pub mod logging;
pub mod middleware;
pub mod platform;

#[cfg(all(feature = "spin", target_arch = "wasm32"))]
use edgezero_adapter_spin::key_value_store::{DEFAULT_MAX_LIST_KEYS, SpinKvStore};
#[cfg(all(feature = "spin", target_arch = "wasm32"))]
use edgezero_core::app::{Hooks, StoreMetadata};
#[cfg(all(feature = "spin", target_arch = "wasm32"))]
use edgezero_core::env_config::EnvConfig;
#[cfg(all(feature = "spin", target_arch = "wasm32"))]
use edgezero_core::store_registry::KvRegistry;
#[cfg(all(feature = "spin", target_arch = "wasm32"))]
use spin_sdk::http::{IntoResponse, Request};
#[cfg(all(feature = "spin", target_arch = "wasm32"))]
use spin_sdk::http_service;

#[cfg(any(test, all(feature = "spin", target_arch = "wasm32")))]
use futures::StreamExt as _;

/// The body of every response this component sends: a buffered body as the
/// router built it, or a streamed body passed to the host frame by frame.
#[cfg(any(test, all(feature = "spin", target_arch = "wasm32")))]
type ResponseBody = http_body_util::Either<
    http_body_util::Full<bytes::Bytes>,
    http_body_util::StreamBody<
        futures::stream::LocalBoxStream<
            'static,
            Result<http_body::Frame<bytes::Bytes>, anyhow::Error>,
        >,
    >,
>;

#[cfg(all(feature = "spin", target_arch = "wasm32"))]
#[http_service]
// FORCED: the Spin `http_service` boundary takes this `anyhow::Result`, the
// error type `edgezero_adapter_spin::run_app` returned before this streaming
// dispatch replaced it, so failures reach the host exactly as they did.
async fn handle(req: Request) -> anyhow::Result<impl IntoResponse> {
    logging::init_logger();
    dispatch::<app::TrustedServerApp>(req).await
}

/// Dispatch a request through the router and hand any streamed body to the
/// host as it is produced.
///
/// This replaces `edgezero_adapter_spin::run_app`, whose response conversion
/// collects a `Body::Stream` into one buffer, capped at 16 MiB, before the
/// host sees a byte. `EdgeZero` keeps the registry construction that wrapper
/// performs private, so the declared KV stores are opened here with the
/// same settings and error, see [`kv_registry`]. Request conversion and
/// routing are `EdgeZero`'s own.
#[cfg(all(feature = "spin", target_arch = "wasm32"))]
async fn dispatch<A: Hooks>(
    req: Request,
) -> anyhow::Result<spin_sdk::http::Response<ResponseBody>> {
    let app = A::build_app();
    let stores = A::stores();
    // `run_app` would also open declared config and secret stores. This app
    // declares neither, so one appearing here must fail loudly rather than
    // reach the router unbound.
    if stores.config.is_some() || stores.secrets.is_some() {
        anyhow::bail!(
            "the streaming Spin dispatch binds declared KV stores alone, \
             but the app declares a config or secret store"
        );
    }
    let env = EnvConfig::from_env();
    let kv_registry = kv_registry(stores.kv, &env).await?;
    let mut core_request = edgezero_adapter_spin::request::into_core_request(req).await?;
    if let Some(registry) = kv_registry {
        core_request.extensions_mut().insert(registry);
    }
    let response = app.router().oneshot(core_request).await?;
    let (parts, body) = response.into_parts();
    Ok(spin_sdk::http::Response::<ResponseBody>::from_parts(
        parts,
        response_body(body),
    ))
}

/// Open every declared KV store, as `EdgeZero`'s Spin dispatch does.
///
/// Each id opens the label `EDGEZERO__STORES__KV__<ID>__NAME` names, with
/// the list cap `EDGEZERO__STORES__KV__<ID>__MAX_LIST_KEYS` sets. A declared
/// store that cannot be opened fails the request, because a declared store
/// is required.
#[cfg(all(feature = "spin", target_arch = "wasm32"))]
async fn kv_registry(
    meta: Option<StoreMetadata>,
    env: &EnvConfig,
) -> anyhow::Result<Option<KvRegistry>> {
    let Some(meta) = meta else {
        return Ok(None);
    };
    let mut by_id = std::collections::BTreeMap::new();
    for id in meta.ids {
        let label = env.store_name("kv", id);
        let max_list_keys = env
            .store_setting("kv", id, "MAX_LIST_KEYS")
            .and_then(|raw| raw.parse::<usize>().ok())
            .unwrap_or(DEFAULT_MAX_LIST_KEYS);
        let store = SpinKvStore::open_with_max_list_keys(&label, max_list_keys)
            .await
            .map_err(|err| {
                anyhow::anyhow!(
                    "Spin KV store '{label}' (id `{id}`) is explicitly configured but could not be opened: {err}"
                )
            })?;
        by_id.insert(
            (*id).to_owned(),
            edgezero_core::key_value_store::KvHandle::new(std::sync::Arc::new(store)),
        );
    }
    Ok(KvRegistry::from_parts(by_id, meta.default.to_owned()))
}

/// The host body for a router body.
///
/// A buffered body stays one buffer. A streamed body becomes one frame per
/// chunk, read only when the host asks for the next frame, so it never
/// holds more than one chunk. A chunk error is passed on as a body error,
/// which aborts the response instead of ending it as though it were
/// complete.
#[cfg(any(test, all(feature = "spin", target_arch = "wasm32")))]
fn response_body(body: edgezero_core::body::Body) -> ResponseBody {
    match body {
        edgezero_core::body::Body::Once(bytes) => {
            http_body_util::Either::Left(http_body_util::Full::new(bytes))
        }
        edgezero_core::body::Body::Stream(stream) => {
            http_body_util::Either::Right(http_body_util::StreamBody::new(
                stream
                    .map(|chunk| chunk.map(http_body::Frame::data))
                    .boxed_local(),
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use futures::FutureExt as _;
    use http_body_util::BodyExt as _;

    #[tokio::test]
    async fn a_streamed_body_reaches_the_host_one_chunk_at_a_time() {
        let (sender, receiver) =
            futures::channel::mpsc::unbounded::<Result<Bytes, std::io::Error>>();
        sender
            .unbounded_send(Ok(Bytes::from_static(b"first")))
            .expect("should queue the first chunk");
        let mut body = response_body(edgezero_core::body::Body::from_stream(receiver));

        let first = body
            .frame()
            .await
            .expect("the first frame should arrive")
            .expect("the first frame should be readable")
            .into_data()
            .expect("the first frame should carry data");
        assert_eq!(first.as_ref(), b"first");
        assert!(
            body.frame().now_or_never().is_none(),
            "should wait for the next chunk rather than end the body"
        );

        sender
            .unbounded_send(Ok(Bytes::from_static(b"second")))
            .expect("should queue the second chunk");
        drop(sender);
        let second = body
            .frame()
            .await
            .expect("the second frame should arrive")
            .expect("the second frame should be readable")
            .into_data()
            .expect("the second frame should carry data");
        assert_eq!(second.as_ref(), b"second");
        assert!(body.frame().await.is_none(), "should end with the stream");
    }

    #[tokio::test]
    async fn a_failed_chunk_aborts_the_host_body() {
        let stream = futures::stream::iter([
            Ok(Bytes::from_static(b"partial")),
            Err(std::io::Error::other("origin connection reset")),
        ]);
        let mut body = response_body(edgezero_core::body::Body::from_stream(stream));

        body.frame()
            .await
            .expect("the chunk before the failure should arrive")
            .expect("the chunk before the failure should be readable");
        let error = body
            .frame()
            .await
            .expect("the failure should surface")
            .expect_err("a failed chunk must not look like the end of the body");
        assert!(
            error.to_string().contains("origin connection reset"),
            "should carry the failure: {error}"
        );
    }

    #[tokio::test]
    async fn a_buffered_body_stays_one_buffer() {
        let body = response_body(edgezero_core::body::Body::from(b"whole".to_vec()));

        assert!(
            matches!(body, http_body_util::Either::Left(_)),
            "a buffered router body should not become a stream"
        );
        let collected = body
            .collect()
            .await
            .expect("a buffered body should be readable")
            .to_bytes();
        assert_eq!(collected.as_ref(), b"whole");
    }
}
