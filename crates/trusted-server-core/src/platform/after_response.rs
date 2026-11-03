//! Small requests a module hands to the platform to send after the reader's
//! response has gone out, on a platform whose adapter offers that lifetime.
//!
//! A platform without a timer cannot bound a request made while a reader
//! waits, but one whose instance keeps running after it sent the response
//! can send it then, so the reader never waits for it. Its adapter declares
//! the handoff once, before it serves, and sends what was handed over after
//! each response. Nothing here is durable: a request that is not sent, or
//! whose answer is lost, is lost, and the module that handed it over must
//! have another path to the same outcome.
//!
//! The handoff is bounded: at most [`MAX_AFTER_RESPONSE_REQUESTS`] requests
//! of at most [`MAX_AFTER_RESPONSE_BODY_BYTES`] each wait at a time, and a
//! request over either bound is refused. One instance serves one request at
//! a time on every platform that declares the handoff, so what waits belongs
//! to the request being served. An adapter drops what is left before a new
//! request starts.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, PoisonError};

use bytes::Bytes;

/// The most requests that wait for one response.
pub const MAX_AFTER_RESPONSE_REQUESTS: usize = 8;

/// The largest body one waiting request carries.
pub const MAX_AFTER_RESPONSE_BODY_BYTES: usize = 8 * 1024;

static SUPPORTED: AtomicBool = AtomicBool::new(false);
static TIMER: AtomicBool = AtomicBool::new(false);
static WAITING: Mutex<Vec<AfterResponseRequest>> = Mutex::new(Vec::new());

/// Declares that every request's services carry a real
/// [`PlatformTimer`](super::PlatformTimer), so a module can bound work a
/// reader waits for. Called once, before serving, by an adapter in exactly
/// the build that installs its timer.
pub fn declare_platform_timer() {
    TIMER.store(true, Ordering::Relaxed);
}

/// Whether the adapter declared a real timer on every request.
#[must_use]
pub fn platform_offers_timer() -> bool {
    TIMER.load(Ordering::Relaxed)
}

/// One request to send after the response, to a backend the module already
/// registered while the reader's request ran.
#[derive(Debug, Clone)]
pub struct AfterResponseRequest {
    /// The request, its body held whole.
    pub request: http::Request<Bytes>,
    /// The backend it goes to.
    pub backend_name: String,
}

/// Declares that the platform sends handed-over requests after each
/// response. Called by an adapter whose instance keeps running after its
/// response is sent, once, before it serves.
pub fn declare_platform_after_response_handoff() {
    SUPPORTED.store(true, Ordering::Relaxed);
}

/// Whether the adapter declared the after-response handoff.
#[must_use]
pub fn platform_offers_after_response_handoff() -> bool {
    SUPPORTED.load(Ordering::Relaxed)
}

/// Hands `request` over to be sent after the response. Whether it was
/// accepted: never where the platform declared no handoff, the request's
/// body is over [`MAX_AFTER_RESPONSE_BODY_BYTES`], or
/// [`MAX_AFTER_RESPONSE_REQUESTS`] already wait.
pub fn hand_over_after_response(request: AfterResponseRequest) -> bool {
    if !platform_offers_after_response_handoff()
        || request.request.body().len() > MAX_AFTER_RESPONSE_BODY_BYTES
    {
        return false;
    }
    let mut waiting = WAITING.lock().unwrap_or_else(PoisonError::into_inner);
    if waiting.len() >= MAX_AFTER_RESPONSE_REQUESTS {
        return false;
    }
    waiting.push(request);
    true
}

/// Takes every request waiting to be sent, for the adapter to send after
/// the response or to drop before a new request starts.
#[must_use]
pub fn take_after_response_requests() -> Vec<AfterResponseRequest> {
    std::mem::take(&mut *WAITING.lock().unwrap_or_else(PoisonError::into_inner))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(body: usize) -> AfterResponseRequest {
        AfterResponseRequest {
            request: http::Request::builder()
                .method("POST")
                .uri("https://worker.example.com/v1/preparation/signals")
                .body(Bytes::from(vec![b'x'; body]))
                .expect("should build a request"),
            backend_name: "worker".to_owned(),
        }
    }

    #[test]
    fn handoff_is_refused_until_declared_then_bounded_and_drained() {
        let _ = take_after_response_requests();
        if !platform_offers_after_response_handoff() {
            assert!(
                !hand_over_after_response(request(1)),
                "should refuse a handoff the platform never declared"
            );
        }
        declare_platform_after_response_handoff();
        assert!(
            !hand_over_after_response(request(MAX_AFTER_RESPONSE_BODY_BYTES + 1)),
            "should refuse a body over the bound"
        );
        for _ in 0..MAX_AFTER_RESPONSE_REQUESTS {
            assert!(
                hand_over_after_response(request(16)),
                "should accept within the bound"
            );
        }
        assert!(
            !hand_over_after_response(request(16)),
            "should refuse past the request bound"
        );
        assert_eq!(
            take_after_response_requests().len(),
            MAX_AFTER_RESPONSE_REQUESTS,
            "should drain every waiting request"
        );
        assert!(
            take_after_response_requests().is_empty(),
            "should hold nothing once drained"
        );
    }
}
