//! Scripted `EdgeZero` outbound client and test clock.
//!
//! [`ScriptedHttpClient`] implements `EdgeZero`'s [`OutboundHttpClient`] for tests. It records
//! every request the [`HttpClient`] facade dispatches, as [`RecordedRequest`], and answers each
//! one from a first-in, first-out script kept per method and URI.
//!
//! Replies arrive at instants on a [`TestClock`] that only the test moves. A reply scripted
//! with [`ScriptedReply::after`] arrives that long after its request entered dispatch, which for a
//! batch is the batch start. A reply that has not arrived by its request's deadline times out at
//! that deadline. A batch drops a slot that ends at or after its cutoff, and the slot is
//! `Unresolved`, as `EdgeZero`'s batch ledger does. A test that waits on a future reply advances
//! the clock between polls, or from a future it polls alongside the waiting one.

use std::collections::{HashMap, VecDeque};
use std::io;
use std::mem;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::task::{Context, Poll, Wake, Waker};
use std::time::Duration;

use bytes::{Bytes, BytesMut};
use edgezero_core::body::{Body, BodyPayload};
use edgezero_core::error::{EdgeError, RequestBodyOverflowOrigin};
use edgezero_core::http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, Uri};
use edgezero_core::outbound::{
    HttpClient, IndependentOutboundProgress, OutboundBatch, OutboundBatchDriverEvent,
    OutboundCachePolicy, OutboundDispatchRequest, OutboundExchangeOwner, OutboundFuture,
    OutboundHead, OutboundHeaderFidelity, OutboundHeaderInput, OutboundHttpClient,
    OutboundImageOptimizerOptions, OutboundPending, OutboundPreparedBatch, OutboundProgressRuntime,
    OutboundProxyMarker, OutboundRequest, OutboundRequestParts, OutboundResponse,
    OutboundResponseContext, OutboundSendMode, OutboundTime, ResponseDecoding, ResponseMode,
};
use edgezero_core::time::{Deadline, DispatchBudget, MonotonicClock, MonotonicInstant};
use futures::future::poll_fn;
use futures::stream::{self, StreamExt as _};

/// Adapter identity the scripted client reports for a proxy marker a request turns on.
const ADAPTER: &str = "scripted";

/// Hand-advanced monotonic time for scripted outbound tests.
///
/// Clones share one instant, which starts an hour after the process clock's current instant and
/// moves only through [`TestClock::advance`]. A deadline taken from the process clock, such as one
/// from `MonotonicClock::default()`, is therefore already expired on this clock, so code that
/// mixes clocks fails its tests. Advancing wakes every timer and scripted reply waiting on the
/// clock, as `EdgeZero` requires of a fake clock.
#[derive(Clone)]
pub struct TestClock {
    state: Arc<Mutex<ClockState>>,
}

/// How far a new [`TestClock`] starts ahead of the process clock.
const PROCESS_CLOCK_OFFSET: Duration = Duration::from_hours(1);

struct ClockState {
    now: MonotonicInstant,
    waiters: Vec<Waker>,
}

impl TestClock {
    /// Creates a clock that stands still until advanced.
    ///
    /// # Panics
    ///
    /// Panics if the process clock cannot be offset by an hour.
    #[must_use]
    pub fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(ClockState {
                now: MonotonicInstant::now()
                    .checked_add(PROCESS_CLOCK_OFFSET)
                    .expect("should offset the test clock from process time"),
                waiters: Vec::new(),
            })),
        }
    }

    /// Returns a [`MonotonicClock`] that reads this clock.
    #[must_use]
    pub fn monotonic_clock(&self) -> MonotonicClock {
        let state = Arc::clone(&self.state);
        MonotonicClock::new(move || lock(&state).now)
    }

    /// Returns an [`OutboundTime`] on this clock.
    ///
    /// Its timers resolve once [`TestClock::advance`] brings the clock to their deadline.
    #[must_use]
    pub fn outbound_time(&self) -> OutboundTime {
        let clock = self.clone();
        OutboundTime::new(self.monotonic_clock(), move |deadline, _paired| {
            let clock = clock.clone();
            Box::pin(poll_fn(move |context| {
                clock
                    .poll_reached(deadline.instant(), context.waker())
                    .map(Ok)
            }))
        })
    }

    /// Moves the clock forward by `by` and wakes everything waiting on it.
    ///
    /// # Panics
    ///
    /// Panics if the new instant does not fit the platform's monotonic clock.
    pub fn advance(&self, by: Duration) {
        let waiters = {
            let mut state = lock(&self.state);
            state.now = state
                .now
                .checked_add(by)
                .expect("should advance the test clock within the platform clock range");
            mem::take(&mut state.waiters)
        };
        waiters.into_iter().for_each(Waker::wake);
    }

    fn now(&self) -> MonotonicInstant {
        lock(&self.state).now
    }

    /// Reports whether the clock has reached `instant`, or arranges a wakeup when it does.
    fn poll_reached(&self, instant: MonotonicInstant, waker: &Waker) -> Poll<()> {
        let mut state = lock(&self.state);
        if state.now >= instant {
            return Poll::Ready(());
        }
        register(&mut state.waiters, waker);
        Poll::Pending
    }
}

impl Default for TestClock {
    fn default() -> Self {
        Self::new()
    }
}

/// One scripted answer to a request.
///
/// A status reply has no headers and an empty body until the builder methods add them. A reply
/// built with [`ScriptedReply::error`] ignores header and body settings.
pub struct ScriptedReply {
    outcome: Result<StatusCode, EdgeError>,
    headers: HeaderMap,
    chunks: Vec<Bytes>,
    delay: Duration,
    hold: Option<Hold>,
    body_polls: Option<Arc<AtomicUsize>>,
}

impl ScriptedReply {
    /// A response with `status`.
    #[must_use]
    pub fn status(status: StatusCode) -> Self {
        Self::with_outcome(Ok(status))
    }

    /// A failure that the request returns unchanged instead of a response.
    #[must_use]
    pub fn error(error: EdgeError) -> Self {
        Self::with_outcome(Err(error))
    }

    /// Appends one response header.
    ///
    /// # Panics
    ///
    /// Panics if `name` or `value` is not a valid header field.
    #[must_use]
    pub fn header(mut self, name: &str, value: &str) -> Self {
        let name =
            HeaderName::from_bytes(name.as_bytes()).expect("should parse the scripted header name");
        let value = HeaderValue::from_str(value).expect("should parse the scripted header value");
        self.headers.append(name, value);
        self
    }

    /// Sets the body, delivered as one chunk.
    #[must_use]
    pub fn body(self, bytes: impl Into<Bytes>) -> Self {
        self.chunks(vec![bytes.into()])
    }

    /// Sets the body as `chunks`, delivered as one stream item each.
    #[must_use]
    pub fn chunks(mut self, chunks: Vec<Bytes>) -> Self {
        self.chunks = chunks;
        self
    }

    /// Makes the reply arrive `delay` after its request enters dispatch, on the test clock.
    ///
    /// A batch slot's request enters dispatch at the batch start.
    #[must_use]
    pub fn after(mut self, delay: Duration) -> Self {
        self.delay = delay;
        self
    }

    /// Holds the reply until the returned [`ReplyRelease`] is released.
    ///
    /// A held reply that is still held at its request's deadline times out there.
    pub fn held(mut self) -> (Self, ReplyRelease) {
        let hold = Hold::default();
        self.hold = Some(hold.clone());
        (self, ReplyRelease { hold })
    }

    /// Counts in `polls` each poll of the body stream, the poll that ends it included.
    ///
    /// A test reads the count to prove how much of the body the code under test read.
    #[must_use]
    pub fn counting_body_polls(mut self, polls: &Arc<AtomicUsize>) -> Self {
        self.body_polls = Some(Arc::clone(polls));
        self
    }

    fn with_outcome(outcome: Result<StatusCode, EdgeError>) -> Self {
        Self {
            outcome,
            headers: HeaderMap::new(),
            chunks: Vec::new(),
            delay: Duration::ZERO,
            hold: None,
            body_polls: None,
        }
    }

    /// Builds the provider head `EdgeZero` finalizes, or returns the scripted error.
    fn into_head(self, context: OutboundResponseContext) -> Result<OutboundHead, EdgeError> {
        let status = self.outcome?;
        let mut chunks = stream::iter(self.chunks.into_iter().map(Ok::<_, EdgeError>));
        let body = match self.body_polls {
            None => chunks.boxed_local(),
            Some(polls) => stream::poll_fn(move |context| {
                polls.fetch_add(1, Ordering::SeqCst);
                chunks.poll_next_unpin(context)
            })
            .boxed_local(),
        };
        OutboundHead::new(
            status,
            OutboundHeaderInput::new(
                ADAPTER,
                self.headers,
                OutboundHeaderFidelity::AdapterVisibleFields,
            ),
            body,
            context,
            OutboundExchangeOwner::new(|| {}),
        )
    }
}

/// Releases one held [`ScriptedReply`].
#[must_use = "a held reply never arrives unless it is released"]
pub struct ReplyRelease {
    hold: Hold,
}

impl ReplyRelease {
    /// Lets the held reply arrive now, or at its [`ScriptedReply::after`] delay if that is later.
    pub fn release(self) {
        self.hold.release();
    }
}

/// One request as the scripted client received it from the facade.
///
/// The fields come from [`OutboundRequestParts`] after preparation, so the proxy marker is
/// resolved to on or off and the headers are normalized.
#[derive(Clone, Debug)]
pub struct RecordedRequest {
    /// Request method.
    pub method: Method,
    /// Canonical request URI.
    pub uri: Uri,
    /// Normalized request headers.
    pub headers: HeaderMap,
    /// Whole request body, collected when the request streamed it.
    pub body: Bytes,
    /// Per-call timeout.
    pub timeout: Option<Duration>,
    /// Connect timeout.
    pub connect_timeout: Option<Duration>,
    /// Caller-supplied absolute deadline.
    pub deadline: Option<Deadline>,
    /// Intermediary cache policy.
    pub cache_policy: OutboundCachePolicy,
    /// Whether response content codings are decoded.
    pub response_decoding: ResponseDecoding,
    /// Whether the response is buffered or streamed.
    pub response_mode: ResponseMode,
    /// Cap on decoded response bytes.
    pub max_decoded_response_bytes: Option<u64>,
    /// Cap on encoded response bytes.
    pub max_encoded_response_bytes: Option<u64>,
    /// Image Optimizer options.
    pub image_optimizer: Option<OutboundImageOptimizerOptions>,
    /// Resolved proxy marker setting.
    pub proxy_marker: OutboundProxyMarker,
    /// Authority sent upstream, from [`OutboundRequest::host_authority`].
    pub host_authority: String,
    /// Whether the request overrides its URI authority upstream.
    pub has_host_authority_override: bool,
}

/// Transport settings a readback test expects one recorded request to carry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExpectedTransport {
    /// Per-call timeout.
    pub timeout: Duration,
    /// Buffered under its cap, or streamed.
    pub response_mode: ResponseMode,
    /// Whether response content codings are decoded.
    pub response_decoding: ResponseDecoding,
    /// Intermediary cache policy.
    pub cache_policy: OutboundCachePolicy,
}

impl RecordedRequest {
    /// Asserts the transport settings every Trusted Server request carries.
    ///
    /// Checks a one second connect timeout, no caller deadline, the proxy marker off, the URI
    /// authority sent upstream with no override, and the timeout, response mode, decoding, and
    /// cache policy in `expected`.
    ///
    /// # Panics
    ///
    /// Panics when any of those settings differs.
    pub fn assert_transport(&self, expected: ExpectedTransport) {
        let request = format!("{} {}", self.method, self.uri);
        assert_eq!(
            self.connect_timeout,
            Some(Duration::from_secs(1)),
            "should bound the connect of {request} at one second"
        );
        assert_eq!(
            self.timeout,
            Some(expected.timeout),
            "should give {request} its constant timeout"
        );
        assert!(
            self.deadline.is_none(),
            "should not give {request} a deadline from a remaining budget"
        );
        assert_eq!(
            (
                self.response_mode,
                self.response_decoding,
                self.cache_policy
            ),
            (
                expected.response_mode,
                expected.response_decoding,
                expected.cache_policy
            ),
            "should give {request} its response mode, decoding, and cache policy"
        );
        assert_eq!(
            self.proxy_marker,
            OutboundProxyMarker::Off,
            "should send {request} with the proxy marker off"
        );
        assert_eq!(
            (
                Some(self.host_authority.as_str()),
                self.has_host_authority_override
            ),
            (
                self.uri.authority().map(http::uri::Authority::as_str),
                false
            ),
            "should send {request} to its URI authority with no override"
        );
    }

    /// Asserts the recorded headers are exactly `expected`, in any order.
    ///
    /// Names compare case-insensitively. `Host`, `Content-Length`, and hop-by-hop fields never
    /// appear here, because `EdgeZero` strips them before dispatch.
    ///
    /// # Panics
    ///
    /// Panics when a header is missing, extra, or carries another value, or when a recorded
    /// value is not UTF-8.
    pub fn assert_headers(&self, expected: &[(&str, &str)]) {
        let mut actual: Vec<(String, String)> = self
            .headers
            .iter()
            .map(|(name, value)| {
                let value = value.to_str().expect("should record UTF-8 header values");
                (name.as_str().to_owned(), value.to_owned())
            })
            .collect();
        let mut expected: Vec<(String, String)> = expected
            .iter()
            .map(|(name, value)| (name.to_ascii_lowercase(), (*value).to_owned()))
            .collect();
        actual.sort();
        expected.sort();
        assert_eq!(
            actual, expected,
            "should send {} {} with exactly the expected headers",
            self.method, self.uri
        );
    }
}

/// A scripted `EdgeZero` outbound client that records requests and answers them from a script.
///
/// Clones share the script, the record, and the clock. Requests that a test never scripted fail
/// with an internal error and are still recorded.
#[derive(Clone)]
pub struct ScriptedHttpClient {
    clock: TestClock,
    forbid: bool,
    state: Arc<Mutex<ScriptState>>,
}

#[derive(Default)]
struct ScriptState {
    replies: HashMap<(Method, String), VecDeque<ScriptedReply>>,
    requests: Vec<RecordedRequest>,
}

impl ScriptedHttpClient {
    /// Creates a client with an empty script whose replies arrive on `clock`.
    #[must_use]
    pub fn new(clock: &TestClock) -> Self {
        Self {
            clock: clock.clone(),
            forbid: false,
            state: Arc::default(),
        }
    }

    /// Creates a client that fails every request it receives.
    ///
    /// Each failure is internal, with a message that starts with `should not send <METHOD>
    /// <uri>`, and each request is still recorded. Call
    /// [`ScriptedHttpClient::assert_no_requests`] to fail the test on any of them.
    #[must_use]
    pub fn forbidding() -> Self {
        Self {
            forbid: true,
            ..Self::new(&TestClock::new())
        }
    }

    /// Queues `reply` after any replies already queued for `method` requests to `uri`.
    ///
    /// # Panics
    ///
    /// Panics if `uri` is not an absolute HTTP or HTTPS URI.
    // Queuing the reply is the effect. The returned reference only lets calls chain.
    #[allow(clippy::must_use_candidate)]
    pub fn respond_to(&self, method: Method, uri: &str, reply: ScriptedReply) -> &Self {
        let canonical = OutboundRequest::get(uri)
            .expect("should parse the scripted URI")
            .uri()
            .to_string();
        if let Some(hold) = &reply.hold {
            hold.attach(&self.clock);
        }
        lock(&self.state)
            .replies
            .entry((method, canonical))
            .or_default()
            .push_back(reply);
        self
    }

    /// Returns a facade over this client with the proxy marker off.
    #[must_use]
    pub fn http_client(&self) -> HttpClient {
        HttpClient::with_client(self.clone()).with_proxy_marker(false)
    }

    /// Returns every request received so far, in arrival order.
    #[must_use]
    pub fn requests(&self) -> Vec<RecordedRequest> {
        lock(&self.state).requests.clone()
    }

    /// Fails the test if any request was received.
    ///
    /// # Panics
    ///
    /// Panics with `should not send <METHOD> <uri>`, naming every received request, if there are
    /// any.
    pub fn assert_no_requests(&self) {
        let sent: Vec<String> = lock(&self.state)
            .requests
            .iter()
            .map(|request| format!("{} {}", request.method, request.uri))
            .collect();
        assert!(sent.is_empty(), "should not send {}", sent.join(", "));
    }

    /// Records the request, takes its reply, and builds the head it arrives with.
    fn schedule(
        &self,
        recorded: RecordedRequest,
        context: OutboundResponseContext,
    ) -> Scheduled<OutboundHead> {
        let reply = self.take_reply(recorded);
        let budget = context.budget();
        let arrival = Arrival {
            at: context
                .entered_at()
                .checked_add(reply.delay)
                .unwrap_or_else(|| budget.deadline.instant()),
            budget,
            hold: reply.hold.clone(),
        };
        Scheduled {
            arrival,
            outcome: reply.into_head(context),
        }
    }

    /// Schedules a buffered request and finalizes its response at once, as eager work does.
    fn schedule_eager(&self, request: OutboundDispatchRequest) -> Scheduled<OutboundResponse> {
        let Dispatched {
            mut recorded,
            body,
            context,
            ..
        } = Dispatched::new(request);
        // The facade hands begin and batch only buffered bodies. It rejects streamed uploads
        // there, and settlement drains a GET or HEAD stream before the provider sees it.
        recorded.body = body
            .into_bytes()
            .expect("should receive a buffered body for begin or batch");
        let Scheduled { arrival, outcome } = self.schedule(recorded, context);
        Scheduled {
            arrival,
            outcome: outcome.and_then(finish_now),
        }
    }

    fn take_reply(&self, recorded: RecordedRequest) -> ScriptedReply {
        let mut state = lock(&self.state);
        let RecordedRequest { method, uri, .. } = &recorded;
        let reply = if self.forbid {
            ScriptedReply::error(internal(format!("should not send {method} {uri}")))
        } else {
            state
                .replies
                .get_mut(&(method.clone(), uri.to_string()))
                .and_then(VecDeque::pop_front)
                .unwrap_or_else(|| {
                    ScriptedReply::error(internal(format!("unscripted request {method} {uri}")))
                })
        };
        state.requests.push(recorded);
        reply
    }
}

impl OutboundHttpClient for ScriptedHttpClient {
    fn begin(&self, request: OutboundDispatchRequest) -> Result<OutboundPending, EdgeError> {
        let scheduled = self.schedule_eager(request);
        Ok(OutboundPending::new(
            Box::pin(scheduled.wait(self.clock.clone())),
            OutboundExchangeOwner::new(|| {}),
        ))
    }

    fn dispatch_head(&self, request: OutboundDispatchRequest) -> OutboundFuture<OutboundHead> {
        let client = self.clone();
        let Dispatched {
            mut recorded,
            body,
            max_request_body_bytes,
            context,
        } = Dispatched::new(request);
        Box::pin(async move {
            recorded.body = collect_upload(body, max_request_body_bytes).await?;
            let scheduled = client.schedule(recorded, context);
            scheduled.wait(client.clock).await
        })
    }

    fn progress(&self) -> Rc<dyn OutboundProgressRuntime> {
        Rc::new(IndependentOutboundProgress)
    }

    fn start_batch_until(&self, batch: OutboundPreparedBatch) -> OutboundBatch {
        let (context, requests) = batch.into_slots();
        if requests.is_empty() {
            return OutboundBatch::cutoff(context);
        }
        let mut driver = BatchDriver {
            clock: self.clock.clone(),
            cutoff: context.observation_cutoff(),
            slots: requests
                .into_iter()
                .map(|(index, request)| (index, self.schedule_eager(request)))
                .collect(),
            late: false,
        };
        OutboundBatch::from_driver(
            context,
            stream::poll_fn(move |context| driver.poll_event(context)),
            OutboundExchangeOwner::new(|| {}),
        )
    }

    fn time(&self) -> OutboundTime {
        self.clock.outbound_time()
    }

    fn validate_supported_options(
        &self,
        _request: &OutboundRequest,
        _mode: OutboundSendMode,
    ) -> Result<(), EdgeError> {
        Ok(())
    }
}

/// A prepared request taken apart for recording.
///
/// `recorded.body` stays empty until the body is collected.
struct Dispatched {
    recorded: RecordedRequest,
    body: Body,
    max_request_body_bytes: u64,
    context: OutboundResponseContext,
}

impl Dispatched {
    fn new(request: OutboundDispatchRequest) -> Self {
        let host_authority = request.request().host_authority().to_owned();
        let has_host_authority_override = request.request().has_host_authority_override();
        let (parts, context) = request.into_parts();
        let OutboundRequestParts {
            body,
            cache_policy,
            connect_timeout,
            deadline,
            headers,
            image_optimizer,
            max_decoded_response_bytes,
            max_encoded_response_bytes,
            max_request_body_bytes,
            method,
            proxy_marker,
            response_decoding,
            response_mode,
            timeout,
            uri,
            ..
        } = parts;
        Self {
            recorded: RecordedRequest {
                method,
                uri,
                headers,
                body: Bytes::new(),
                timeout,
                connect_timeout,
                deadline,
                cache_policy,
                response_decoding,
                response_mode,
                max_decoded_response_bytes,
                max_encoded_response_bytes,
                image_optimizer,
                proxy_marker,
                host_authority,
                has_host_authority_override,
            },
            body,
            max_request_body_bytes,
            context,
        }
    }
}

/// A scheduled exchange: when its reply arrives, and what it arrives with.
struct Scheduled<T> {
    arrival: Arrival,
    outcome: Result<T, EdgeError>,
}

impl<T> Scheduled<T> {
    /// Waits on `clock` for the reply or the deadline, whichever comes first.
    async fn wait(self, clock: TestClock) -> Result<T, EdgeError> {
        let terminal = poll_fn(|context| self.arrival.poll(&clock, context.waker())).await;
        self.settle(terminal)
    }

    fn settle(self, terminal: Terminal) -> Result<T, EdgeError> {
        match terminal {
            Terminal::Arrived(_) => self.outcome,
            Terminal::TimedOut(_) => Err(EdgeError::gateway_timeout_caused(
                "scripted reply did not arrive before its deadline",
                self.arrival.budget.cause,
            )),
        }
    }
}

/// When one scripted reply becomes observable on the test clock.
struct Arrival {
    at: MonotonicInstant,
    budget: DispatchBudget,
    hold: Option<Hold>,
}

/// The first event that ends a scripted exchange, at its test-clock instant.
#[derive(Clone, Copy)]
enum Terminal {
    Arrived(MonotonicInstant),
    TimedOut(MonotonicInstant),
}

impl Terminal {
    fn instant(self) -> MonotonicInstant {
        match self {
            Self::Arrived(instant) | Self::TimedOut(instant) => instant,
        }
    }
}

impl Arrival {
    /// Reports the terminal event once the clock reaches it, or arranges a wakeup.
    ///
    /// A reply arriving at or after the budget deadline times out at the deadline, as `EdgeZero`
    /// treats a deadline at or before now as expired.
    fn poll(&self, clock: &TestClock, waker: &Waker) -> Poll<Terminal> {
        let arrived = match &self.hold {
            None => Some(self.at),
            Some(hold) => match hold.poll_release(waker) {
                Poll::Pending => None,
                Poll::Ready(released_at) => {
                    Some(released_at.map_or(self.at, |released| released.max(self.at)))
                }
            },
        };
        let deadline = self.budget.deadline.instant();
        let terminal = match arrived {
            Some(at) if at < deadline => Terminal::Arrived(at),
            _ => Terminal::TimedOut(deadline),
        };
        clock
            .poll_reached(terminal.instant(), waker)
            .map(|()| terminal)
    }
}

/// Emits batch slot outcomes in test-clock order, as `EdgeZero`'s Axum batch driver does.
struct BatchDriver {
    clock: TestClock,
    cutoff: Deadline,
    slots: Vec<(usize, Scheduled<OutboundResponse>)>,
    late: bool,
}

impl BatchDriver {
    fn poll_event(&mut self, context: &mut Context<'_>) -> Poll<Option<OutboundBatchDriverEvent>> {
        let mut earliest: Option<(usize, Terminal)> = None;
        for (position, (_, slot)) in self.slots.iter().enumerate() {
            if let Poll::Ready(terminal) = slot.arrival.poll(&self.clock, context.waker())
                && earliest.is_none_or(|(_, first)| terminal.instant() < first.instant())
            {
                earliest = Some((position, terminal));
            }
        }
        let Some((position, terminal)) = earliest else {
            if !self.slots.is_empty() {
                return Poll::Pending;
            }
            // The ledger drops a slot that ends at or after the cutoff, so a batch that lost one
            // must end with an explicit cutoff rather than a clean end of stream.
            return Poll::Ready(
                mem::take(&mut self.late).then_some(OutboundBatchDriverEvent::Cutoff),
            );
        };
        let (index, slot) = self.slots.remove(position);
        let completed_at = terminal.instant();
        self.late |= self.cutoff.is_expired_at(completed_at)
            && !slot
                .arrival
                .budget
                .own_deadline_expired_before(self.cutoff, completed_at);
        Poll::Ready(Some(OutboundBatchDriverEvent::Item {
            index,
            completed_at,
            outcome: slot.settle(terminal),
        }))
    }
}

/// Shared state between a held reply and its [`ReplyRelease`].
#[derive(Clone, Default)]
struct Hold(Arc<Mutex<HoldState>>);

#[derive(Default)]
struct HoldState {
    clock: Option<TestClock>,
    release: Release,
    waiters: Vec<Waker>,
}

#[derive(Clone, Copy, Default)]
enum Release {
    #[default]
    Held,
    /// Released at this instant, or before the reply was scripted on any clock.
    Released(Option<MonotonicInstant>),
}

impl Hold {
    fn attach(&self, clock: &TestClock) {
        let mut state = lock(&self.0);
        if state.clock.is_none() {
            state.clock = Some(clock.clone());
        }
    }

    fn release(&self) {
        let waiters = {
            let mut state = lock(&self.0);
            state.release = Release::Released(state.clock.as_ref().map(TestClock::now));
            mem::take(&mut state.waiters)
        };
        waiters.into_iter().for_each(Waker::wake);
    }

    /// Reports when the reply was released, or arranges a wakeup for its release.
    fn poll_release(&self, waker: &Waker) -> Poll<Option<MonotonicInstant>> {
        let mut state = lock(&self.0);
        match state.release {
            Release::Released(released_at) => Poll::Ready(released_at),
            Release::Held => {
                register(&mut state.waiters, waker);
                Poll::Pending
            }
        }
    }
}

/// Records whether a future woke itself while being polled.
struct WakeFlag(AtomicBool);

impl Wake for WakeFlag {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.0.store(true, Ordering::SeqCst);
    }
}

/// Finalizes a scripted head at the current test-clock instant, without an executor.
///
/// Scripted bodies are in memory, so the finalizer only ever waits on itself, yielding between
/// chunks and waking itself. Polling it until it stops waking itself finishes it the way a real
/// provider's eagerly scheduled task would.
fn finish_now(head: OutboundHead) -> Result<OutboundResponse, EdgeError> {
    let woken = Arc::new(WakeFlag(AtomicBool::new(true)));
    let waker = Waker::from(Arc::clone(&woken));
    let mut context = Context::from_waker(&waker);
    let mut finish = head.finish();
    while woken.0.swap(false, Ordering::SeqCst) {
        if let Poll::Ready(outcome) = finish.as_mut().poll(&mut context) {
            return outcome;
        }
    }
    Err(internal(
        "scripted reply finalizer waited on something outside the script".to_owned(),
    ))
}

/// Collects a request body, enforcing the upload cap on a streamed body as providers do.
///
/// The facade already checked a buffered body against the cap.
async fn collect_upload(body: Body, max_request_body_bytes: u64) -> Result<Bytes, EdgeError> {
    let (payload, _egress_owner) = body.into_parts();
    let mut source = match payload {
        BodyPayload::Once(bytes) => return Ok(bytes),
        BodyPayload::Stream(source) => source,
    };
    let mut collected = BytesMut::new();
    while let Some(chunk) = source.next().await {
        collected.extend_from_slice(&chunk?);
        if u64::try_from(collected.len()).unwrap_or(u64::MAX) > max_request_body_bytes {
            return Err(EdgeError::payload_too_large(
                RequestBodyOverflowOrigin::OutboundUpload,
            ));
        }
    }
    Ok(collected.freeze())
}

fn internal(message: String) -> EdgeError {
    EdgeError::internal(io::Error::other(message))
}

/// Adds `waker` to `waiters` unless an equivalent waker is already there.
fn register(waiters: &mut Vec<Waker>, waker: &Waker) {
    if !waiters.iter().any(|waiter| waiter.will_wake(waker)) {
        waiters.push(waker.clone());
    }
}

/// Locks `mutex`, recovering the data if a panicking test poisoned it.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::task::Wake;

    use edgezero_core::error::BudgetSource;
    use edgezero_core::outbound::{
        OutboundBatchCheck, OutboundBatchTermination, OutboundCollectedSlot, OutboundImageFormat,
        OutboundImageOptimizerRegion,
    };
    use futures::executor::block_on;

    use super::*;

    const ORIGIN: &str = "https://example.com";

    /// Counts wakeups, so tests can prove that advancing or releasing wakes a waiter.
    struct WakeCount(AtomicUsize);

    impl Wake for WakeCount {
        fn wake(self: Arc<Self>) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn uri(path: &str) -> String {
        format!("{ORIGIN}{path}")
    }

    fn get(path: &str) -> OutboundRequest {
        OutboundRequest::get(uri(path)).expect("should build the GET request")
    }

    fn poll_once<T>(future: &mut OutboundFuture<T>, waker: &Waker) -> Poll<Result<T, EdgeError>> {
        future.as_mut().poll(&mut Context::from_waker(waker))
    }

    fn body_of(response: &OutboundResponse) -> &[u8] {
        response
            .body()
            .as_bytes()
            .expect("should buffer the response body")
    }

    #[test]
    fn records_method_uri_headers_and_options() {
        let clock = TestClock::new();
        let client = ScriptedHttpClient::new(&clock);
        client
            .respond_to(
                Method::POST,
                &uri("/bid"),
                ScriptedReply::status(StatusCode::OK),
            )
            .respond_to(
                Method::GET,
                &uri("/image.png"),
                ScriptedReply::status(StatusCode::OK),
            );
        let http = client.http_client();
        let deadline = clock
            .monotonic_clock()
            .deadline_after(Duration::from_secs(60));
        let bid = OutboundRequest::post(uri("/bid"))
            .expect("should build the POST request")
            .header("x-example", "one")
            .expect("should add the request header")
            .host_authority_override("origin.example.com")
            .expect("should override the host authority")
            .body(Bytes::from_static(b"{\"id\":1}"))
            .timeout(Duration::from_secs(2))
            .connect_timeout(Duration::from_secs(1))
            .deadline(deadline)
            .cache_policy(OutboundCachePolicy::Bypass)
            .response_decoding(ResponseDecoding::Original)
            .max_decoded_response_bytes(4096)
            .max_encoded_response_bytes(2048);
        let options = OutboundImageOptimizerOptions::new()
            .width(320)
            .expect("should set the image width")
            .format(OutboundImageFormat::WebP)
            .region(OutboundImageOptimizerRegion::UsEast);
        let image = get("/image.png")
            .image_optimizer(options.clone())
            .stream_response();

        block_on(http.send(bid)).expect("should answer the bid request");
        block_on(http.send(image)).expect("should answer the image request");

        assert!(
            !http.proxy_marker_enabled(),
            "should hand out a client with the proxy marker off"
        );
        let recorded = client.requests();
        assert_eq!(recorded.len(), 2, "should record both requests");
        let bid = &recorded[0];
        assert_eq!(
            (
                &bid.method,
                bid.uri.to_string(),
                bid.headers.get("x-example"),
                &bid.body
            ),
            (
                &Method::POST,
                uri("/bid"),
                Some(&HeaderValue::from_static("one")),
                &Bytes::from_static(b"{\"id\":1}")
            ),
            "should record the method, URI, headers, and body"
        );
        assert_eq!(
            (
                bid.timeout,
                bid.connect_timeout,
                bid.deadline.map(|recorded| recorded.instant())
            ),
            (
                Some(Duration::from_secs(2)),
                Some(Duration::from_secs(1)),
                Some(deadline.instant())
            ),
            "should record the timeouts and deadline"
        );
        assert_eq!(
            (
                bid.cache_policy,
                bid.response_decoding,
                bid.max_decoded_response_bytes,
                bid.max_encoded_response_bytes
            ),
            (
                OutboundCachePolicy::Bypass,
                ResponseDecoding::Original,
                Some(4096),
                Some(2048)
            ),
            "should record the cache policy, decoding mode, and response caps"
        );
        assert!(
            matches!(bid.response_mode, ResponseMode::Buffered { .. }),
            "should record a buffered response mode"
        );
        assert_eq!(
            (
                bid.proxy_marker,
                &bid.image_optimizer,
                bid.host_authority.as_str(),
                bid.has_host_authority_override
            ),
            (OutboundProxyMarker::Off, &None, "origin.example.com", true),
            "should record the resolved marker, no image options, and the authority override"
        );
        let image = &recorded[1];
        assert_eq!(
            (
                &image.method,
                image.response_mode,
                &image.image_optimizer,
                image.host_authority.as_str(),
                image.has_host_authority_override
            ),
            (
                &Method::GET,
                ResponseMode::Streamed,
                &Some(options),
                "example.com",
                false
            ),
            "should record the image mode, its Image Optimizer options, and the URI authority"
        );
    }

    #[test]
    fn replies_in_fifo_order_per_method_and_uri() {
        let clock = TestClock::new();
        let client = ScriptedHttpClient::new(&clock);
        client
            .respond_to(
                Method::GET,
                &uri("/a"),
                ScriptedReply::status(StatusCode::OK).body("first"),
            )
            .respond_to(
                Method::GET,
                &uri("/a"),
                ScriptedReply::status(StatusCode::CREATED).body("second"),
            )
            .respond_to(
                Method::POST,
                &uri("/a"),
                ScriptedReply::status(StatusCode::ACCEPTED),
            )
            .respond_to(
                Method::GET,
                &uri("/b"),
                ScriptedReply::status(StatusCode::OK).header("x-reply", "b"),
            );
        let http = client.http_client();
        let post = OutboundRequest::post(uri("/a")).expect("should build the POST request");

        let first = block_on(http.send(get("/a"))).expect("should answer the first GET");
        let posted = block_on(http.send(post)).expect("should answer the POST");
        let other = block_on(http.send(get("/b"))).expect("should answer the other URI");
        let second = block_on(http.send(get("/a"))).expect("should answer the second GET");
        let unscripted =
            block_on(http.send(get("/a"))).expect_err("should fail once the script runs out");

        assert_eq!(
            first.status(),
            StatusCode::OK,
            "should reply first in first out"
        );
        assert_eq!(body_of(&first), b"first", "should send the first body");
        assert_eq!(
            posted.status(),
            StatusCode::ACCEPTED,
            "should keep a separate queue per method"
        );
        assert_eq!(
            other.headers().get("x-reply").map(HeaderValue::as_bytes),
            Some(&b"b"[..]),
            "should keep a separate queue per URI and send scripted headers"
        );
        assert_eq!(
            second.status(),
            StatusCode::CREATED,
            "should send the second reply next"
        );
        assert_eq!(body_of(&second), b"second", "should send the second body");
        assert!(
            matches!(unscripted, EdgeError::Internal { .. }),
            "should fail an unscripted request as internal, got {unscripted:?}"
        );
        assert!(
            unscripted
                .inner()
                .is_some_and(|inner| inner.to_string().starts_with("unscripted request")),
            "should name the failure, got {unscripted}"
        );
        let recorded = client.requests();
        assert_eq!(
            recorded.len(),
            5,
            "should record every request, unscripted too"
        );
        assert_eq!(
            recorded[4].uri.to_string(),
            uri("/a"),
            "should record the unscripted request"
        );
    }

    #[test]
    fn chunked_reply_streams_each_chunk() {
        let clock = TestClock::new();
        let client = ScriptedHttpClient::new(&clock);
        let chunks = vec![
            Bytes::from_static(b"one"),
            Bytes::from_static(b"two"),
            Bytes::from_static(b"three"),
        ];
        for _ in 0..2 {
            client.respond_to(
                Method::GET,
                &uri("/stream"),
                ScriptedReply::status(StatusCode::OK).chunks(chunks.clone()),
            );
        }
        let http = client.http_client();

        let streamed = block_on(http.send(get("/stream").stream_response()))
            .expect("should answer the streamed request");
        let Ok(stream) = streamed.into_body().into_stream() else {
            panic!("should stream the response body");
        };
        let received: Vec<Bytes> = block_on(
            stream
                .map(|chunk| chunk.expect("should read each chunk"))
                .collect(),
        );
        let buffered =
            block_on(http.send(get("/stream"))).expect("should answer the buffered request");

        assert_eq!(
            received, chunks,
            "should deliver each scripted chunk as its own stream item"
        );
        assert_eq!(
            body_of(&buffered),
            b"onetwothree",
            "should join the chunks for a buffered response"
        );
    }

    #[test]
    fn held_reply_resolves_only_after_release() {
        let clock = TestClock::new();
        let client = ScriptedHttpClient::new(&clock);
        let (begun_reply, begun_release) =
            ScriptedReply::status(StatusCode::OK).body("late").held();
        let (sent_reply, sent_release) = ScriptedReply::status(StatusCode::OK).held();
        client
            .respond_to(Method::GET, &uri("/begun"), begun_reply)
            .respond_to(Method::GET, &uri("/sent"), sent_reply);
        let http = client.http_client();
        let wakes = Arc::new(WakeCount(AtomicUsize::new(0)));
        let waker = Waker::from(Arc::clone(&wakes));

        let mut pending = block_on(http.begin(get("/begun"))).expect("should begin the request");
        let mut sent = http.send(get("/sent"));
        clock.advance(Duration::from_millis(10));
        let begun_while_held = pending.is_ready();
        let sent_while_held = poll_once(&mut sent, &waker);
        let wakes_while_held = wakes.0.load(Ordering::SeqCst);
        begun_release.release();
        sent_release.release();

        assert!(
            !begun_while_held && sent_while_held.is_pending(),
            "should keep held requests pending, even as time passes"
        );
        assert!(
            wakes.0.load(Ordering::SeqCst) > wakes_while_held,
            "should wake the waiting send when released"
        );
        assert!(
            pending.is_ready(),
            "should resolve the begun request once released"
        );
        let begun = block_on(pending.collect()).expect("should collect the released reply");
        assert_eq!(body_of(&begun), b"late", "should deliver the held body");
        assert!(
            matches!(poll_once(&mut sent, &waker), Poll::Ready(Ok(_))),
            "should resolve the send once released"
        );
    }

    #[test]
    fn batch_drops_reply_arriving_after_cutoff() {
        let clock = TestClock::new();
        let client = ScriptedHttpClient::new(&clock);
        client
            .respond_to(
                Method::GET,
                &uri("/fast"),
                ScriptedReply::status(StatusCode::OK).after(Duration::from_millis(100)),
            )
            .respond_to(
                Method::GET,
                &uri("/slow"),
                ScriptedReply::status(StatusCode::OK).after(Duration::from_millis(600)),
            );
        let cutoff = clock
            .monotonic_clock()
            .deadline_after(Duration::from_millis(500));

        let mut batch = client
            .http_client()
            .start_batch_until(vec![get("/fast"), get("/slow")], cutoff);
        let before = batch.check().expect("should check the batch");
        clock.advance(Duration::from_millis(600));
        let (slots, termination) = block_on(batch.collect())
            .expect("should collect the batch")
            .into_parts();

        assert!(
            matches!(before, OutboundBatchCheck::Pending),
            "should observe nothing before a reply arrives, got {before:?}"
        );
        assert_eq!(
            termination,
            OutboundBatchTermination::Cutoff,
            "should end the batch at the cutoff"
        );
        let [
            OutboundCollectedSlot::Resolved(fast),
            OutboundCollectedSlot::Unresolved,
        ] = &slots[..]
        else {
            panic!("should resolve the fast slot and leave the slow one unresolved: {slots:?}");
        };
        assert_eq!(
            fast.elapsed,
            Duration::from_millis(100),
            "should time the slot from batch start to its arrival, not its collection"
        );
        assert_eq!(
            fast.outcome
                .as_ref()
                .expect("should answer the fast slot")
                .status(),
            StatusCode::OK,
            "should deliver the fast reply"
        );
    }

    #[test]
    fn error_reply_fails_only_its_slot() {
        let clock = TestClock::new();
        let client = ScriptedHttpClient::new(&clock);
        client
            .respond_to(
                Method::GET,
                &uri("/ok"),
                ScriptedReply::status(StatusCode::OK).body("bid"),
            )
            .respond_to(
                Method::GET,
                &uri("/broken"),
                ScriptedReply::error(EdgeError::bad_gateway("scripted bidder failure")),
            );
        let cutoff = clock
            .monotonic_clock()
            .deadline_after(Duration::from_secs(1));

        let (slots, termination) = block_on(
            client
                .http_client()
                .send_all_until(vec![get("/ok"), get("/broken")], cutoff),
        )
        .expect("should collect the batch")
        .into_parts();

        assert_eq!(
            termination,
            OutboundBatchTermination::Completed,
            "should complete once every slot resolves"
        );
        let [
            OutboundCollectedSlot::Resolved(ok),
            OutboundCollectedSlot::Resolved(broken),
        ] = &slots[..]
        else {
            panic!("should resolve both slots: {slots:?}");
        };
        assert_eq!(
            body_of(ok.outcome.as_ref().expect("should answer the healthy slot")),
            b"bid",
            "should deliver the healthy slot's body"
        );
        let error = broken
            .outcome
            .as_ref()
            .expect_err("should fail the broken slot");
        assert!(
            matches!(error, EdgeError::BadGateway { .. }),
            "should keep the scripted error, got {error:?}"
        );
        assert_eq!(
            error.message(),
            "scripted bidder failure",
            "should keep the scripted error message"
        );
    }

    #[test]
    fn forbidding_client_fails_and_records_every_request() {
        let client = ScriptedHttpClient::forbidding();
        client.respond_to(
            Method::GET,
            &uri("/forbidden"),
            ScriptedReply::status(StatusCode::OK),
        );

        let error = block_on(client.http_client().send(get("/forbidden")))
            .expect_err("should fail every request");

        assert!(
            matches!(error, EdgeError::Internal { .. }),
            "should fail as internal, got {error:?}"
        );
        assert!(
            error.inner().is_some_and(|inner| inner
                .to_string()
                .starts_with("should not send GET https://example.com/forbidden")),
            "should name the forbidden request, got {error}"
        );
        let recorded = client.requests();
        assert_eq!(recorded.len(), 1, "should record the forbidden request");
        assert_eq!(recorded[0].method, Method::GET, "should record its method");
        assert_eq!(
            recorded[0].uri.to_string(),
            uri("/forbidden"),
            "should record its URI"
        );
    }

    #[test]
    fn forbidding_client_without_requests_passes_its_check() {
        let client = ScriptedHttpClient::forbidding();

        client.assert_no_requests();

        assert!(client.requests().is_empty(), "should record nothing");
    }

    #[test]
    fn send_times_out_when_its_reply_arrives_after_the_deadline() {
        let clock = TestClock::new();
        let client = ScriptedHttpClient::new(&clock);
        client.respond_to(
            Method::GET,
            &uri("/slow"),
            ScriptedReply::status(StatusCode::OK).after(Duration::from_secs(2)),
        );

        let mut sent = client
            .http_client()
            .send(get("/slow").timeout(Duration::from_secs(1)));
        let before = poll_once(&mut sent, Waker::noop());
        clock.advance(Duration::from_secs(1));
        let after = poll_once(&mut sent, Waker::noop());

        assert!(before.is_pending(), "should wait for the reply");
        assert!(
            matches!(
                after,
                Poll::Ready(Err(EdgeError::GatewayTimeout {
                    cause: BudgetSource::PerCallTimeout,
                    ..
                }))
            ),
            "should time out at the per-call deadline, got {after:?}"
        );
    }

    #[test]
    fn test_clock_wakes_timers_when_advanced() {
        let clock = TestClock::new();
        let time = clock.outbound_time();
        let mut wait = time.wait_until(time.clock().deadline_after(Duration::from_millis(100)));
        let wakes = Arc::new(WakeCount(AtomicUsize::new(0)));
        let waker = Waker::from(Arc::clone(&wakes));

        let before = poll_once(&mut wait, &waker);
        clock.advance(Duration::from_millis(99));
        let woken_early = wakes.0.load(Ordering::SeqCst);
        let early = poll_once(&mut wait, &waker);
        clock.advance(Duration::from_millis(1));
        let at_deadline = poll_once(&mut wait, &waker);

        assert!(before.is_pending(), "should wait before the deadline");
        assert!(woken_early > 0, "should wake the timer when advanced");
        assert!(
            early.is_pending(),
            "should keep waiting short of the deadline"
        );
        assert!(
            matches!(at_deadline, Poll::Ready(Ok(()))),
            "should resolve once the clock reaches the deadline, got {at_deadline:?}"
        );
    }

    #[test]
    fn process_clock_deadline_is_already_expired_on_the_test_clock() {
        let clock = TestClock::new();
        let test_now = clock.monotonic_clock().now();

        let process_deadline = MonotonicClock::default().deadline_after(Duration::from_secs(60));
        let test_deadline = clock
            .monotonic_clock()
            .deadline_after(Duration::from_secs(60));

        assert!(
            process_deadline.is_expired_at(test_now),
            "should expire a process-clock deadline on the test clock, so mixed clocks fail tests"
        );
        assert!(
            !test_deadline.is_expired_at(test_now),
            "should keep a test-clock deadline open on the test clock"
        );
    }
}
