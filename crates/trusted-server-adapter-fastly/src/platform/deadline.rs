//! Deadline-bounded collection of Fastly auction responses.

use std::io::Read as _;
use std::time::Duration;

use error_stack::{Report, ResultExt as _};
use fastly::handle::BodyHandle;
use fastly::http::request::{PendingRequest, PollResult};
use trusted_server_core::platform::{
    PlatformError, PlatformPendingRequest, PlatformResponse, PlatformSelectResult,
};
use web_time::Instant;

use super::{MAX_PLATFORM_RESPONSE_BODY_BYTES, fastly_response_to_platform, response_carries_body};

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
    Headers(Box<Option<PendingRequest>>),
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
        let pending = pending.downcast::<PendingRequest>().map_err(|_| {
            Report::new(PlatformError::HttpClient).attach(format!(
                "invalid Fastly deadline pending handle for backend '{backend_name}'"
            ))
        })?;
        Ok(Self {
            backend_name,
            request_method,
            state: Some(PendingState::Headers(Box::new(Some(pending)))),
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
            _ => 0,
        }
    }

    fn poll(
        &mut self,
        deadline: Instant,
        available_buffer_bytes: usize,
    ) -> Result<Option<PlatformResponse>, Report<PlatformError>> {
        let state = self
            .state
            .take()
            .expect("should retain pending collection state");
        let state = match state {
            PendingState::Headers(mut slot) => {
                match slot.take().expect("should retain a pending request").poll() {
                    PollResult::Pending(pending) => {
                        *slot = Some(pending);
                        self.state = Some(PendingState::Headers(slot));
                        return Ok(None);
                    }
                    PollResult::Done(Err(error)) => {
                        self.backend_name = error.backend_name().to_owned();
                        return Err(
                            Report::new(PlatformError::HttpClient).attach(error.to_string())
                        );
                    }
                    PollResult::Done(Ok(mut response)) => {
                        if let Some(backend_name) = response.get_backend_name() {
                            self.backend_name = backend_name.to_owned();
                        }
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
            mut response,
            mut body,
            mut bytes,
        } = state
        else {
            unreachable!("headers are polled before buffering bodies");
        };
        if Instant::now() >= deadline || !body_is_ready(&body)? || Instant::now() >= deadline {
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
            response.set_body(bytes);
            return fastly_response_to_platform(*response, self.backend_name.clone(), false, false)
                .map(Some);
        }
        if bytes.len() + read > MAX_PLATFORM_RESPONSE_BODY_BYTES {
            return Err(Report::new(PlatformError::HttpClient)
                .attach("origin response body exceeds response body limit"));
        }
        if read > available_buffer_bytes {
            return Err(Report::new(PlatformError::HttpClient)
                .attach("origin response body exceeds aggregate auction buffer capacity"));
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

/// Collect one fully buffered response before the absolute deadline.
///
/// Dropping response bodies closes their host handles. Fastly has no pending
/// request cancellation ABI: unfinished header requests are abandoned and
/// never waited on past the auction deadline.
///
/// Partial bodies share a 10 MiB payload budget. A provider whose next chunk
/// would exceed that capacity fails with an attributed HTTP client error;
/// its buffered bytes are released and its siblings remain collectible.
/// Completed response payload plus retained partial bodies fits the same budget.
///
/// # Errors
///
/// Returns an error for invalid handles or a failure of the timed wait hostcall.
/// Provider errors are attributed in the returned collection result.
pub(super) fn select_until(
    pending_requests: Vec<PlatformPendingRequest>,
    deadline: Instant,
) -> Result<Option<PlatformSelectResult>, Report<PlatformError>> {
    if Instant::now() >= deadline {
        return Ok(None);
    }
    if pending_requests.is_empty() {
        return Err(Report::new(PlatformError::HttpClient)
            .attach("select_until called with an empty pending_requests list"));
    }
    let mut pending = pending_requests
        .into_iter()
        .map(DeadlinePending::from_platform)
        .collect::<Result<Vec<_>, _>>()?;
    let mut buffered_bytes: usize = pending.iter().map(DeadlinePending::buffered_bytes).sum();
    if buffered_bytes > MAX_TOTAL_BUFFERED_BODY_BYTES {
        return Err(Report::new(PlatformError::HttpClient)
            .attach("resumed auction responses exceed aggregate buffer capacity"));
    }
    loop {
        for index in 0..pending.len() {
            if Instant::now() >= deadline {
                return Ok(None);
            }
            let previous_bytes = pending[index].buffered_bytes();
            let result =
                pending[index].poll(deadline, MAX_TOTAL_BUFFERED_BODY_BYTES - buffered_bytes);
            buffered_bytes = buffered_bytes - previous_bytes + pending[index].buffered_bytes();
            // A host event arriving at the deadline must not make a late bid eligible.
            if Instant::now() >= deadline {
                return Ok(None);
            }
            let ready = match result {
                Ok(None) => continue,
                Ok(Some(response)) => Ok(response),
                Err(error) => Err(error),
            };
            let completed = pending.swap_remove(index);
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
        wait_for_progress(&pending, deadline)?;
    }
}

fn wait_for_progress(
    pending: &[DeadlinePending],
    deadline: Instant,
) -> Result<(), Report<PlatformError>> {
    let Some(mut remaining) = deadline.checked_duration_since(Instant::now()) else {
        return Ok(());
    };
    if remaining.is_zero() {
        return Ok(());
    }
    let mut bodies = Vec::new();
    for pending in pending {
        match &pending.state {
            Some(PendingState::Headers(_)) => {
                // High-level PendingRequest keeps its host handle private. Poll
                // at most 1ms apart without spinning while headers are pending.
                remaining = remaining.min(Duration::from_millis(1));
            }
            Some(PendingState::Body { body, .. }) => {
                // SAFETY: these handles remain owned by pending throughout select.
                bodies.push(unsafe { body.as_u32() });
            }
            None => {}
        }
    }
    let timeout_ms = u32::try_from(remaining.as_millis())
        .unwrap_or(u32::MAX)
        .max(1);
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

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::super::{FastlyPlatformHttpClient, MAX_PLATFORM_RESPONSE_BODY_BYTES};
    use super::{DeadlinePending, PendingState};
    use edgezero_core::body::Body;
    use trusted_server_core::platform::{
        PlatformHttpClient, PlatformHttpRequest, PlatformPendingRequest,
    };
    use web_time::Instant;

    #[test]
    fn expired_deadline_does_not_accept_a_pending_result() {
        let result = futures::executor::block_on(FastlyPlatformHttpClient.select_until(
            vec![PlatformPendingRequest::new(42_u32).with_backend_name("example")],
            Instant::now(),
        ))
        .expect("should report expiration before inspecting handles");
        assert!(result.is_none(), "should discard expired pending requests");
    }

    #[test]
    fn deadline_collection_rejects_malformed_handles_with_backend_context() {
        let result = futures::executor::block_on(FastlyPlatformHttpClient.select_until(
            vec![PlatformPendingRequest::new(42_u32).with_backend_name("example")],
            Instant::now() + Duration::from_secs(1),
        ));
        let error = result.expect_err("should reject a malformed pending handle");
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
    fn expired_deadline_discards_an_already_ready_body() {
        let pending = buffered_pending(b"ready".to_vec(), "example");
        let result = futures::executor::block_on(
            FastlyPlatformHttpClient.select_until(vec![pending], Instant::now()),
        )
        .expect("should discard an expired ready response");
        assert!(
            result.is_none(),
            "should never accept a ready response after expiration"
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
    fn aggregate_body_limit_rejects_growth_and_preserves_partial_siblings() {
        let half_budget = MAX_PLATFORM_RESPONSE_BODY_BYTES / 2;
        let partial = |unread: Vec<u8>, backend: &str| {
            let mut pending = DeadlinePending::from_platform(buffered_pending(unread, backend))
                .expect("should construct a partially buffered response");
            if let Some(PendingState::Body { bytes, .. }) = &mut pending.state {
                *bytes = vec![b'x'; half_budget];
            }
            pending.into_platform()
        };
        let pending = vec![
            partial(vec![b'x'], "growing"),
            partial(Vec::new(), "sibling"),
        ];
        let deadline = Instant::now() + Duration::from_secs(10);
        let result =
            futures::executor::block_on(FastlyPlatformHttpClient.select_until(pending, deadline))
                .expect("should return an attributed capacity failure")
                .expect("should reject excess buffer growth before deadline");
        assert!(
            result.ready.is_err(),
            "should reject growth beyond aggregate buffer capacity"
        );
        assert_eq!(
            result.failed_backend_name.as_deref(),
            Some("growing"),
            "should attribute the capacity failure to the growing provider"
        );
        assert_eq!(
            result.remaining.len(),
            1,
            "should preserve the partial sibling response"
        );
        let result = futures::executor::block_on(
            FastlyPlatformHttpClient.select_until(result.remaining, deadline),
        )
        .expect("should resume the preserved sibling")
        .expect("should complete its already buffered body");
        let response = result.ready.expect("should retain the successful sibling");
        assert_eq!(
            response.backend_name.as_deref(),
            Some("sibling"),
            "should preserve backend correlation"
        );
        let (_, body) = response.response.into_parts();
        let bytes = body
            .into_bytes()
            .expect("should retain a buffered response body");
        assert_eq!(
            bytes.len(),
            half_budget,
            "should preserve all previously buffered sibling bytes"
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
    #[ignore = "requires the local origin started by tests/deadline_origin.py"]
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
    #[ignore = "requires the local origin started by tests/deadline_origin.py"]
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
    #[ignore = "requires the local origin started by tests/deadline_origin.py"]
    fn network_fast_sibling_completes_while_another_body_trickles() {
        let slow = network_pending("trickle", "GET");
        let fast = network_pending("fast", "GET");
        let started = Instant::now();
        let result = futures::executor::block_on(
            FastlyPlatformHttpClient
                .select_until(vec![slow, fast], started + Duration::from_millis(200)),
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
    #[ignore = "requires the local origin started by tests/deadline_origin.py"]
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
