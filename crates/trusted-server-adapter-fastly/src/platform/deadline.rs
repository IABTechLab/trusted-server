//! Deadline-bounded collection of Fastly auction responses.

use error_stack::{Report, ResultExt as _};
use fastly::handle::{BodyHandle, PendingRequestHandle, PollHandleResult};
use std::io::Read as _;
use trusted_server_core::platform::{
    PlatformError, PlatformPendingRequest, PlatformResponse, PlatformSelectResult,
};
use web_time::Instant;

use super::{
    CacheBypassPending, MAX_PLATFORM_RESPONSE_BODY_BYTES, buffered_response_to_platform,
    fastly_response_to_platform, response_carries_body,
};

// Bound payload retained across all partial bodies, as well as each response.
const MAX_TOTAL_BUFFERED_BODY_BYTES: usize = MAX_PLATFORM_RESPONSE_BODY_BYTES;

// Keep partially buffered responses alongside requests still awaiting headers so
// an early header with a slow body cannot hold up a completed sibling response.
struct DeadlinePending {
    backend_name: String,
    request_method: Option<edgezero_core::http::Method>,
    state: Option<PendingState>,
}

enum PendingState {
    Headers(Option<PendingRequestHandle>),
    Ready(Result<PlatformResponse, Report<PlatformError>>),
    Body {
        response: Box<fastly::Response>,
        body: BodyHandle,
        bytes: Vec<u8>,
    },
}

impl DeadlinePending {
    fn from_platform(pending: PlatformPendingRequest) -> Result<Self, Report<PlatformError>> {
        if pending.stream_response() {
            return Err(Report::new(PlatformError::HttpClient)
                .attach("stream-marked pending request requires direct wait"));
        }
        let request_method = pending.request_method().cloned();
        let pending = match pending.downcast::<Self>() {
            Ok(mut pending) => {
                pending.request_method = request_method.or(pending.request_method);
                return Ok(pending);
            }
            Err(pending) => pending,
        };
        let backend_name = pending.backend_name().unwrap_or("<unknown>").to_owned();
        // The marker is constructed only after set_pass(true), which selects
        // HostCaching in fastly 0.13.1's Request::get_caching_mode. Its direct
        // handle conversion therefore cannot encounter a guest caching state.
        let pending = pending.downcast::<CacheBypassPending>().map_err(|_| {
            Report::new(PlatformError::Unsupported).attach(format!(
                "deadline collection requires a cache-bypass request for backend '{backend_name}'"
            ))
        })?;
        let pending = PendingRequestHandle::from(pending.0);
        Ok(Self {
            backend_name,
            request_method,
            state: Some(PendingState::Headers(Some(pending))),
        })
    }

    fn into_platform(self) -> PlatformPendingRequest {
        let backend_name = self.backend_name.clone();
        let request_method = self.request_method.clone();
        let pending = PlatformPendingRequest::new(self).with_backend_name(backend_name);
        match request_method {
            Some(method) => pending.with_response_handling(false, method),
            None => pending,
        }
    }

    fn request_is_head(&self, response: &fastly::Response) -> Result<bool, Report<PlatformError>> {
        if let Some(method) = &self.request_method {
            return Ok(method == edgezero_core::http::Method::HEAD);
        }
        response
            .get_backend_request()
            .map(|request| request.get_method() == fastly::http::Method::HEAD)
            .ok_or_else(|| {
                Report::new(PlatformError::HttpClient).attach(format!(
                    "deadline response for backend '{}' has no originating request or method hint",
                    self.backend_name
                ))
            })
    }

    fn buffered_bytes(&self) -> usize {
        match &self.state {
            Some(PendingState::Body { bytes, .. }) => bytes.len(),
            Some(PendingState::Ready(Ok(response))) => {
                response.response.body().as_bytes().map_or(0, <[u8]>::len)
            }
            _ => 0,
        }
    }

    fn poll(&mut self) -> Result<Option<PlatformResponse>, Report<PlatformError>> {
        let state = self
            .state
            .take()
            .expect("should retain pending collection state");
        let state = match state {
            PendingState::Ready(result) => return result.map(Some),
            PendingState::Headers(mut slot) => {
                match slot.take().expect("should retain a pending request").poll() {
                    PollHandleResult::Pending(pending) => {
                        slot = Some(pending);
                        self.state = Some(PendingState::Headers(slot));
                        return Ok(None);
                    }
                    PollHandleResult::Done(Err(error)) => {
                        return Err(
                            Report::new(PlatformError::HttpClient).attach(error.to_string())
                        );
                    }
                    PollHandleResult::Done(Ok((response_handle, body_handle))) => {
                        let mut response =
                            fastly::Response::from_handles(response_handle, body_handle);
                        let request_is_head = self.request_is_head(&response)?;
                        if !response_carries_body(request_is_head, response.get_status()) {
                            return fastly_response_to_platform(
                                response,
                                self.backend_name.clone(),
                                false,
                                request_is_head,
                            )
                            .map(Some);
                        }
                        if response
                            .get_header("content-length")
                            .and_then(|value| value.to_str().ok())
                            .and_then(|value| value.trim().parse::<u64>().ok())
                            .is_some_and(|length| length > MAX_PLATFORM_RESPONSE_BODY_BYTES as u64)
                        {
                            return Err(Report::new(PlatformError::HttpClient)
                                .attach("origin Content-Length exceeds response body limit"));
                        }
                        let body = response.take_body().into_handle();
                        PendingState::Body {
                            response: Box::new(response),
                            body,
                            bytes: Vec::new(),
                        }
                    }
                }
            }
            state => state,
        };
        let PendingState::Body {
            response,
            mut body,
            mut bytes,
        } = state
        else {
            unreachable!("headers are polled before buffering bodies");
        };
        if !body_is_ready(&body)? {
            self.state = Some(PendingState::Body {
                response,
                body,
                bytes,
            });
            return Ok(None);
        }
        let mut chunk = [0_u8; 8192];
        let read = body
            .read(&mut chunk)
            .change_context(PlatformError::HttpClient)?;
        if read == 0 {
            return buffered_response_to_platform(&response, self.backend_name.clone(), bytes)
                .map(Some);
        }
        if bytes.len() + read > MAX_PLATFORM_RESPONSE_BODY_BYTES {
            return Err(Report::new(PlatformError::HttpClient)
                .attach("origin response body exceeds response body limit"));
        }
        if bytes.len() + read > bytes.capacity() {
            let capacity = bytes
                .capacity()
                .max(chunk.len())
                .saturating_mul(2)
                .min(MAX_PLATFORM_RESPONSE_BODY_BYTES);
            bytes.reserve_exact(capacity - bytes.len());
        }
        bytes.extend_from_slice(&chunk[..read]);
        self.state = Some(PendingState::Body {
            response,
            body,
            bytes,
        });
        Ok(None)
    }
}

fn body_is_ready(body: &BodyHandle) -> Result<bool, Report<PlatformError>> {
    let mut ready = 0;
    // SAFETY: the live owned body retains the handle while this hostcall borrows it.
    unsafe { fastly_sys::fastly_async_io::is_ready(body.as_u32(), &mut ready) }
        .result()
        .map_err(|status| {
            Report::new(PlatformError::HttpClient)
                .attach(format!("body readiness hostcall failed: {status:?}"))
        })?;
    Ok(ready != 0)
}

/// Collect one fully buffered response without waiting past the absolute deadline.
///
/// Dropping response bodies closes their host handles. Fastly has no pending
/// request cancellation ABI: unfinished header requests are abandoned and
/// never waited on past the auction deadline. If collection starts after the
/// deadline, one nonblocking snapshot preserves already available responses.
///
/// Partial bodies share a 10 MiB payload budget. If a chunk exceeds that
/// capacity, the largest partial response fails with an attributed HTTP client
/// error; its buffered bytes are released and smaller siblings remain collectible.
/// Completed response payload plus retained partial bodies fits the same budget.
///
/// # Errors
///
/// Returns an error for an empty request list, invalid aggregate buffer state,
/// or a failure of the timed wait hostcall. Individual provider errors, including
/// unsupported pending handles, are attributed in the returned collection result.
pub(super) fn select_until(
    pending_requests: Vec<PlatformPendingRequest>,
    deadline: Instant,
) -> Result<Option<PlatformSelectResult>, Report<PlatformError>> {
    if pending_requests.is_empty() {
        return Err(Report::new(PlatformError::HttpClient)
            .attach("select_until called with an empty pending_requests list"));
    }
    let mut pending = pending_requests
        .into_iter()
        .map(|pending| {
            let backend_name = pending.backend_name().unwrap_or("<unknown>").to_owned();
            let request_method = pending.request_method().cloned();
            match DeadlinePending::from_platform(pending) {
                Ok(pending) => pending,
                Err(error) => DeadlinePending {
                    backend_name,
                    request_method,
                    state: Some(PendingState::Ready(Err(error))),
                },
            }
        })
        .collect::<Vec<_>>();
    let mut buffered_bytes: usize = pending.iter().map(DeadlinePending::buffered_bytes).sum();
    if buffered_bytes > MAX_TOTAL_BUFFERED_BODY_BYTES {
        return Err(Report::new(PlatformError::HttpClient)
            .attach("resumed auction responses exceed aggregate buffer capacity"));
    }
    let expired_at_entry = Instant::now() >= deadline;
    if expired_at_entry {
        // Take one nonblocking snapshot. Fully buffered results are cached;
        // responses that would require a wait are abandoned, never retried.
        for index in 0..pending.len() {
            loop {
                let previous = pending[index].buffered_bytes();
                let result = pending[index].poll();
                let progressed =
                    matches!(result, Ok(None)) && pending[index].buffered_bytes() > previous;
                match result {
                    Ok(None) if progressed => {}
                    Ok(None) => {
                        pending[index].state = None;
                    }
                    result => {
                        pending[index].state = Some(PendingState::Ready(match result {
                            Ok(Some(response)) => Ok(response),
                            Err(error) => Err(error),
                            Ok(None) => unreachable!("pending results handled above"),
                        }));
                    }
                }
                buffered_bytes = buffered_bytes - previous + pending[index].buffered_bytes();
                enforce_aggregate_budget(&mut pending, &mut buffered_bytes);
                if !progressed || !matches!(pending[index].state, Some(PendingState::Body { .. })) {
                    break;
                }
            }
        }
        pending.retain(|item| item.state.is_some());
    }
    loop {
        for index in 0..pending.len() {
            if !expired_at_entry && Instant::now() >= deadline {
                // A timed host wait may wake with a ready event at the cutoff.
                // Finish with the same nonblocking snapshot as expired entry.
                return select_until(
                    pending
                        .into_iter()
                        .map(DeadlinePending::into_platform)
                        .collect(),
                    deadline,
                );
            }
            let previous_bytes = pending[index].buffered_bytes();
            let result = pending[index].poll();
            if let Ok(Some(response)) = result {
                pending[index].state = Some(PendingState::Ready(Ok(response)));
            } else if let Err(error) = result {
                pending[index].state = Some(PendingState::Ready(Err(error)));
            }
            buffered_bytes = buffered_bytes - previous_bytes + pending[index].buffered_bytes();
            enforce_aggregate_budget(&mut pending, &mut buffered_bytes);
            // Capacity eviction may have failed an earlier large sibling.
            let ready_index = pending
                .iter()
                .position(|item| matches!(item.state, Some(PendingState::Ready(_))));
            let Some(ready_index) = ready_index else {
                continue;
            };
            let mut completed = pending.swap_remove(ready_index);
            let Some(PendingState::Ready(ready)) = completed.state.take() else {
                unreachable!("should select a ready result");
            };
            let failed_backend_name = ready.as_ref().err().map(|_| completed.backend_name);
            return Ok(Some(PlatformSelectResult {
                ready,
                remaining: pending
                    .into_iter()
                    .map(DeadlinePending::into_platform)
                    .collect(),
                failed_backend_name,
            }));
        }
        if expired_at_entry || pending.is_empty() {
            return Ok(None);
        }
        wait_for_progress(&pending, deadline)?;
    }
}

fn enforce_aggregate_budget(pending: &mut [DeadlinePending], buffered_bytes: &mut usize) {
    if *buffered_bytes <= MAX_TOTAL_BUFFERED_BODY_BYTES {
        return;
    }
    // The overflow is at most one read chunk. Evict the largest partial body
    // first, preserving completed responses and healthy smaller siblings.
    let largest = pending
        .iter()
        .enumerate()
        .filter(|(_, item)| matches!(item.state, Some(PendingState::Body { .. })))
        .max_by_key(|(_, item)| item.buffered_bytes())
        .or_else(|| {
            pending
                .iter()
                .enumerate()
                .max_by_key(|(_, item)| item.buffered_bytes())
        })
        .map(|(index, _)| index)
        .expect("should retain the response exceeding buffer capacity");
    *buffered_bytes -= pending[largest].buffered_bytes();
    pending[largest].state = Some(PendingState::Ready(Err(Report::new(
        PlatformError::HttpClient,
    )
    .attach("largest partial response exceeds aggregate auction buffer capacity"))));
}

fn wait_for_progress(
    pending: &[DeadlinePending],
    deadline: Instant,
) -> Result<(), Report<PlatformError>> {
    let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
        return Ok(());
    };
    if remaining.is_zero() {
        return Ok(());
    }
    let mut bodies = Vec::new();
    for pending in pending {
        match &pending.state {
            Some(PendingState::Headers(Some(handle))) => bodies.push(handle.as_u32()),
            Some(PendingState::Body { body, .. }) => {
                // SAFETY: these handles remain owned by pending throughout select.
                bodies.push(unsafe { body.as_u32() });
            }
            _ => {}
        }
    }
    if bodies.is_empty() {
        return Ok(());
    }
    let timeout_ms = host_select_timeout_ms(remaining);
    let mut ready_index = 0;
    // SAFETY: bodies is a live slice, and ready_index is writable. A positive
    // timeout is essential: the host interprets zero as an unbounded wait.
    unsafe {
        fastly_sys::fastly_async_io::select(
            bodies.as_ptr(),
            bodies.len(),
            timeout_ms,
            &mut ready_index,
        )
    }
    .result()
    .map_err(|status| {
        Report::new(PlatformError::HttpClient)
            .attach(format!("timed select hostcall failed: {status:?}"))
    })?;
    Ok(())
}

/// Convert a remaining budget into the timed-select hostcall argument.
///
/// The host treats a zero timeout as unbounded, so sub-millisecond budgets
/// round up to one millisecond instead.
fn host_select_timeout_ms(remaining: std::time::Duration) -> u32 {
    u32::try_from(remaining.as_millis())
        .unwrap_or(u32::MAX)
        .max(1)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::super::{FastlyPlatformHttpClient, MAX_PLATFORM_RESPONSE_BODY_BYTES};
    use super::{DeadlinePending, PendingState, host_select_timeout_ms};

    #[test]
    fn sub_millisecond_budget_never_requests_an_unbounded_host_wait() {
        assert_eq!(
            host_select_timeout_ms(Duration::from_micros(500)),
            1,
            "should round a sub-millisecond budget up instead of passing 0"
        );
        assert_eq!(
            host_select_timeout_ms(Duration::from_millis(25)),
            25,
            "should pass whole-millisecond budgets through unchanged"
        );
        assert_eq!(
            host_select_timeout_ms(Duration::from_secs(u64::MAX)),
            u32::MAX,
            "should saturate budgets beyond the hostcall range"
        );
    }
    use edgezero_core::body::Body;
    use trusted_server_core::platform::{
        PlatformHttpClient, PlatformHttpRequest, PlatformPendingRequest,
    };
    use web_time::Instant;

    #[test]
    fn expired_deadline_attributes_an_invalid_pending_handle() {
        let result = futures::executor::block_on(FastlyPlatformHttpClient.select_until(
            vec![PlatformPendingRequest::new(42_u32).with_backend_name("example")],
            Instant::now(),
        ))
        .expect("should isolate invalid handles even in the nonblocking snapshot")
        .expect("should report the invalid pending handle");
        assert_eq!(
            result.failed_backend_name.as_deref(),
            Some("example"),
            "should attribute the invalid pending handle"
        );
        let error = result
            .ready
            .expect_err("should reject the invalid pending handle");
        assert!(
            matches!(
                error.current_context(),
                trusted_server_core::platform::PlatformError::Unsupported
            ),
            "should reject unsupported deadline handles without panicking"
        );
    }

    #[test]
    fn deadline_collection_rejects_malformed_handles_with_backend_context() {
        let result = futures::executor::block_on(FastlyPlatformHttpClient.select_until(
            vec![PlatformPendingRequest::new(42_u32).with_backend_name("example")],
            Instant::now() + Duration::from_secs(1),
        ));
        let result = result
            .expect("should isolate malformed pending handles")
            .expect("should return the malformed provider's failure");
        assert_eq!(
            result.failed_backend_name.as_deref(),
            Some("example"),
            "should attribute the malformed handle"
        );
        let error = result
            .ready
            .expect_err("should reject a malformed pending handle");
        assert!(
            format!("{error:?}").contains("example"),
            "should identify the malformed handle's backend"
        );
    }

    fn buffered_pending(bytes: Vec<u8>, backend_name: &str) -> PlatformPendingRequest {
        let mut response = fastly::Response::from_body(bytes);
        let body = response.take_body().into_handle();
        DeadlinePending {
            backend_name: backend_name.to_owned(),
            request_method: Some(edgezero_core::http::Method::GET),
            state: Some(PendingState::Body {
                response: Box::new(response),
                body,
                bytes: Vec::new(),
            }),
        }
        .into_platform()
    }

    #[test]
    fn unsupported_provider_does_not_discard_a_healthy_sibling() {
        let deadline = Instant::now() + Duration::from_secs(1);
        let failed = PlatformPendingRequest::new(42_u32).with_backend_name("unsupported");
        let healthy = buffered_pending(b"healthy".to_vec(), "healthy");
        let result = futures::executor::block_on(
            FastlyPlatformHttpClient.select_until(vec![failed, healthy], deadline),
        )
        .expect("should isolate a provider with an unsupported pending handle")
        .expect("should report the failed provider");
        assert_eq!(
            result.failed_backend_name.as_deref(),
            Some("unsupported"),
            "should attribute the conversion error to its provider"
        );
        assert!(
            result.ready.is_err(),
            "should report the unsupported handle"
        );
        let result = futures::executor::block_on(
            FastlyPlatformHttpClient.select_until(result.remaining, deadline),
        )
        .expect("should continue collecting the healthy sibling")
        .expect("should complete the healthy sibling");
        let response = result.ready.expect("should retain the healthy response");
        assert_eq!(
            response.backend_name.as_deref(),
            Some("healthy"),
            "should preserve the unaffected provider's bid"
        );
    }

    #[test]
    fn expired_deadline_preserves_an_already_ready_body() {
        let pending = buffered_pending(b"ready".to_vec(), "example");
        let result = futures::executor::block_on(
            FastlyPlatformHttpClient.select_until(vec![pending], Instant::now()),
        )
        .expect("should snapshot an expired ready response")
        .expect("should preserve an already complete response without waiting");
        let response = result
            .ready
            .expect("should retain a successful buffered response");
        assert_eq!(
            response.backend_name.as_deref(),
            Some("example"),
            "should retain the ready response's backend"
        );
        assert_eq!(
            response.response.body().as_bytes(),
            Some(b"ready".as_slice()),
            "should retain the complete ready payload"
        );
    }

    #[test]
    fn remaining_handle_retains_the_original_request_method_hint() {
        let pending = buffered_pending(b"body".to_vec(), "example")
            .with_response_handling(false, edgezero_core::http::Method::HEAD);
        let resumed = DeadlinePending::from_platform(pending)
            .expect("should resume a deadline pending request");
        let remaining = resumed.into_platform();
        assert_eq!(
            remaining.request_method(),
            Some(&edgezero_core::http::Method::HEAD),
            "should retain the HEAD method through remaining-handle reconstruction"
        );
        let resumed =
            DeadlinePending::from_platform(remaining).expect("should resume the remaining handle");
        let response = fastly::Response::from_body("advisory body");
        assert!(
            response.get_backend_request().is_none(),
            "should exercise a response without SDK originating-request metadata"
        );
        assert!(
            resumed
                .request_is_head(&response)
                .expect("should use the preserved method hint"),
            "should retain HEAD body semantics after resuming without response origin metadata"
        );
    }

    #[test]
    fn missing_request_method_and_response_origin_returns_typed_error() {
        let pending = buffered_pending(b"body".to_vec(), "example");
        let mut pending = DeadlinePending::from_platform(pending)
            .expect("should inspect a buffered pending request");
        pending.request_method = None;
        let response = fastly::Response::new();
        let error = pending
            .request_is_head(&response)
            .expect_err("should reject a response whose request method is unknown");
        assert!(
            matches!(
                error.current_context(),
                trusted_server_core::platform::PlatformError::HttpClient
            ),
            "should return a typed HTTP client error"
        );
        assert!(
            format!("{error:?}").contains("example"),
            "should identify the backend whose originating method is missing"
        );
    }

    #[test]
    fn completed_body_moves_into_platform_without_a_payload_copy() {
        let bytes = vec![b'x'; 32768];
        let pointer = bytes.as_ptr();
        let response = super::buffered_response_to_platform(
            &fastly::Response::new(),
            "example".to_owned(),
            bytes,
        )
        .expect("should move a buffered response into the platform");
        assert_eq!(
            response
                .response
                .body()
                .as_bytes()
                .expect("should retain a buffered body")
                .as_ptr(),
            pointer,
            "should retain the original allocation without a Fastly host-body roundtrip"
        );
    }

    #[test]
    fn body_collection_is_fair_and_preserves_partial_siblings() {
        let large = vec![b'x'; 32768];
        let pending = vec![
            buffered_pending(large.clone(), "large"),
            buffered_pending(b"fast".to_vec(), "fast"),
        ];
        let deadline = Instant::now() + Duration::from_secs(10);
        let result =
            futures::executor::block_on(FastlyPlatformHttpClient.select_until(pending, deadline))
                .expect("should collect one response")
                .expect("should complete before deadline");
        let response = result
            .ready
            .expect("should collect the short response first");
        assert_eq!(
            response.backend_name.as_deref(),
            Some("fast"),
            "should give siblings a fair opportunity to complete"
        );
        assert_eq!(
            result.remaining.len(),
            1,
            "should retain the partial response"
        );
        let result = futures::executor::block_on(
            FastlyPlatformHttpClient.select_until(result.remaining, deadline),
        )
        .expect("should resume body collection")
        .expect("should finish the remaining body");
        let response = result.ready.expect("should finish the partial body");
        let (_, body) = response.response.into_parts();
        let bytes = body
            .into_bytes()
            .expect("should read the complete buffered body");
        assert_eq!(
            bytes.as_ref(),
            large.as_slice(),
            "should preserve all bytes across collector calls"
        );
    }

    #[test]
    fn body_at_the_exact_limit_remains_eligible() {
        let pending = buffered_pending(vec![b'x'; MAX_PLATFORM_RESPONSE_BODY_BYTES], "exact");
        let result = futures::executor::block_on(
            FastlyPlatformHttpClient
                .select_until(vec![pending], Instant::now() + Duration::from_secs(10)),
        )
        .expect("should collect an exactly sized body")
        .expect("should complete before the deadline");
        let response = result
            .ready
            .expect("should accept the configured body-size boundary");
        assert_eq!(
            response
                .response
                .body()
                .as_bytes()
                .expect("should retain buffered bytes")
                .len(),
            MAX_PLATFORM_RESPONSE_BODY_BYTES,
            "should accept the exact limit without evicting its EOF probe"
        );
    }

    #[test]
    fn incremental_body_limit_reports_the_failed_backend() {
        let pending = buffered_pending(vec![b'x'; MAX_PLATFORM_RESPONSE_BODY_BYTES + 1], "large");
        let result = futures::executor::block_on(
            FastlyPlatformHttpClient
                .select_until(vec![pending], Instant::now() + Duration::from_secs(30)),
        )
        .expect("should return a provider failure")
        .expect("should reject the body before deadline");
        assert!(
            result.ready.is_err(),
            "should enforce the incremental body cap"
        );
        assert_eq!(
            result.failed_backend_name.as_deref(),
            Some("large"),
            "should attribute body conversion failures"
        );
    }

    #[test]
    fn aggregate_body_pressure_evicts_largest_partial_provider() {
        let mut large =
            DeadlinePending::from_platform(buffered_pending(vec![b'x'; 16384], "large"))
                .expect("should construct a large partial response");
        if let Some(PendingState::Body { bytes, .. }) = &mut large.state {
            *bytes = vec![b'x'; MAX_PLATFORM_RESPONSE_BODY_BYTES - 1];
        }
        let pending = vec![
            buffered_pending(b"healthy".to_vec(), "healthy"),
            large.into_platform(),
        ];
        let deadline = Instant::now() + Duration::from_secs(10);
        let result =
            futures::executor::block_on(FastlyPlatformHttpClient.select_until(pending, deadline))
                .expect("should return a capacity result")
                .expect("should reject excess growth");
        assert!(
            result.ready.is_err(),
            "should reject excess aggregate buffering"
        );
        assert_eq!(
            result.failed_backend_name.as_deref(),
            Some("large"),
            "should attribute capacity pressure to the largest partial response"
        );
        let result = futures::executor::block_on(
            FastlyPlatformHttpClient.select_until(result.remaining, deadline),
        )
        .expect("should retain the healthy provider")
        .expect("should complete the healthy provider");
        assert_eq!(
            result
                .ready
                .expect("should preserve the healthy bid")
                .backend_name
                .as_deref(),
            Some("healthy"),
            "should avoid blaming the healthy provider for another provider's large body"
        );
    }

    #[test]
    fn expired_snapshot_retains_only_ready_results_across_calls() {
        let pending = vec![
            buffered_pending(b"one".to_vec(), "one"),
            buffered_pending(b"two".to_vec(), "two"),
        ];
        let deadline = Instant::now();
        let result =
            futures::executor::block_on(FastlyPlatformHttpClient.select_until(pending, deadline))
                .expect("should snapshot ready results")
                .expect("should return first ready result");
        assert!(
            result.ready.is_ok(),
            "should preserve a complete ready response"
        );
        let result = futures::executor::block_on(
            FastlyPlatformHttpClient.select_until(result.remaining, deadline),
        )
        .expect("should use cached ready results")
        .expect("should return second ready result");
        assert!(
            result.ready.is_ok(),
            "should preserve the second complete response"
        );
    }

    fn network_pending(path: &str, method: &str) -> PlatformPendingRequest {
        let request = edgezero_core::http::request_builder()
            .method(method)
            .uri(format!("http://example.com/{path}"))
            .body(Body::empty())
            .expect("should build fixture request");
        futures::executor::block_on(
            FastlyPlatformHttpClient.send_async(
                PlatformHttpRequest::new(request, "deadline_origin").with_cache_bypass(),
            ),
        )
        .expect("should launch fixture request")
    }

    #[test]
    #[ignore = "requires the local origin started by tests/deadline-origin.mjs"]
    fn network_slow_origin_preserves_ready_bid_without_waiting_for_stalled_sibling() {
        let fast = network_pending("fast", "GET");
        let stalled = network_pending("stalled", "GET");
        let deadline = Instant::now() + Duration::from_millis(40);
        std::thread::sleep(Duration::from_millis(100));
        let started = Instant::now();
        let result = futures::executor::block_on(
            FastlyPlatformHttpClient.select_until(vec![stalled, fast], deadline),
        )
        .expect("should snapshot already ready bids")
        .expect("should retain the complete fast response");
        assert!(
            result.ready.is_ok(),
            "should preserve a bid available after a slow publisher origin"
        );
        assert!(
            result.remaining.is_empty(),
            "should abandon the stalled sibling in the nonblocking snapshot"
        );
        assert!(
            started.elapsed() < Duration::from_millis(100),
            "should never wait for the stalled sibling after expiration"
        );
    }

    #[test]
    #[ignore = "requires the local origin started by tests/deadline-origin.mjs"]
    fn network_stalled_headers_stop_at_deadline() {
        let pending = network_pending("stalled", "GET");
        let started = Instant::now();
        let result = futures::executor::block_on(
            FastlyPlatformHttpClient
                .select_until(vec![pending], started + Duration::from_millis(80)),
        )
        .expect("should stop waiting for stalled headers");
        assert!(
            result.is_none(),
            "should expire while headers remain pending"
        );
        assert!(
            started.elapsed() < Duration::from_millis(500),
            "should bound the synchronous header wait"
        );
    }

    #[test]
    #[ignore = "requires the local origin started by tests/deadline-origin.mjs"]
    fn network_trickling_body_stops_at_deadline() {
        let pending = network_pending("trickle", "GET");
        let started = Instant::now();
        let result = futures::executor::block_on(
            FastlyPlatformHttpClient
                .select_until(vec![pending], started + Duration::from_millis(80)),
        )
        .expect("should stop buffering a trickling body");
        assert!(
            result.is_none(),
            "should expire before the trickling body completes"
        );
        assert!(
            started.elapsed() < Duration::from_millis(500),
            "should bound body reads at the auction deadline"
        );
    }

    #[test]
    #[ignore = "requires the local origin started by tests/deadline-origin.mjs"]
    fn network_fast_sibling_completes_while_another_body_trickles() {
        let slow = network_pending("trickle", "GET");
        let fast = network_pending("fast", "GET");
        let request = edgezero_core::http::request_builder()
            .uri("http://example.com/fast")
            .body(Body::empty())
            .expect("should build the request missing cache bypass");
        let unsupported = futures::executor::block_on(
            FastlyPlatformHttpClient
                .send_async(PlatformHttpRequest::new(request, "deadline_origin")),
        )
        .expect("should dispatch the request missing cache bypass");
        let started = Instant::now();
        let deadline = started + Duration::from_millis(200);
        let result = futures::executor::block_on(
            FastlyPlatformHttpClient.select_until(vec![unsupported, slow, fast], deadline),
        )
        .expect("should isolate a request missing cache bypass")
        .expect("should return the unsupported provider's failure");
        assert_eq!(
            result.failed_backend_name.as_deref(),
            Some("deadline_origin"),
            "should attribute missing cache bypass to that request"
        );
        let error = result
            .ready
            .expect_err("should reject the unsupported handle");
        assert!(
            matches!(
                error.current_context(),
                trusted_server_core::platform::PlatformError::Unsupported
            ),
            "should reject SDK guest-caching handles before direct handle conversion"
        );
        let result = futures::executor::block_on(
            FastlyPlatformHttpClient.select_until(result.remaining, deadline),
        )
        .expect("should collect the completed sibling")
        .expect("should finish before deadline");
        let response = result.ready.expect("should collect the fast body");
        let (_, body) = response.response.into_parts();
        let bytes = body.into_bytes().expect("should read the fast body");
        assert_eq!(
            bytes.as_ref(),
            b"fast",
            "should avoid starvation by a trickling sibling"
        );
        assert_eq!(
            result.remaining.len(),
            1,
            "should retain the incomplete sibling"
        );
    }

    #[test]
    #[ignore = "requires the local origin started by tests/deadline-origin.mjs"]
    fn network_head_skips_the_body_and_its_declared_length_limit() {
        let pending = network_pending("head", "HEAD");
        let result = futures::executor::block_on(
            FastlyPlatformHttpClient
                .select_until(vec![pending], Instant::now() + Duration::from_secs(1)),
        )
        .expect("should collect HEAD response")
        .expect("should finish before deadline");
        assert!(
            result.ready.is_ok(),
            "should accept HEAD with an advisory large content-length"
        );
    }
}
