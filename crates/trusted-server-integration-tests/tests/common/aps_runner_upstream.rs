use crate::common::runtime::{TestError, TestResult};
use error_stack::{Report, ResultExt as _};
use std::collections::VecDeque;
use std::io::{Read as _, Write as _};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

const MAX_REQUEST_HEAD_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone)]
pub struct ResponseWrite {
    delay_before: Duration,
    bytes: Vec<u8>,
}

impl ResponseWrite {
    #[must_use]
    pub fn now(bytes: impl Into<Vec<u8>>) -> Self {
        Self {
            delay_before: Duration::ZERO,
            bytes: bytes.into(),
        }
    }

    #[must_use]
    pub fn after(delay_before: Duration, bytes: impl Into<Vec<u8>>) -> Self {
        Self {
            delay_before,
            bytes: bytes.into(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct FictionalResponse {
    writes: Vec<ResponseWrite>,
}

impl FictionalResponse {
    #[must_use]
    pub fn raw(writes: Vec<ResponseWrite>) -> Self {
        Self { writes }
    }

    #[must_use]
    pub fn fixed(status: &str, headers: &[(&str, &str)], body: impl AsRef<[u8]>) -> Self {
        let body = body.as_ref();
        let mut response = format!("HTTP/1.1 {status}\r\nConnection: close\r\n").into_bytes();
        for (name, value) in headers {
            response.extend_from_slice(name.as_bytes());
            response.extend_from_slice(b": ");
            response.extend_from_slice(value.as_bytes());
            response.extend_from_slice(b"\r\n");
        }
        response.extend_from_slice(b"\r\n");
        response.extend_from_slice(body);
        Self::raw(vec![ResponseWrite::now(response)])
    }

    #[must_use]
    pub fn chunked(
        status: &str,
        headers: &[(&str, &str)],
        chunks: Vec<(Duration, Vec<u8>)>,
    ) -> Self {
        let mut head =
            format!("HTTP/1.1 {status}\r\nConnection: close\r\nTransfer-Encoding: chunked\r\n")
                .into_bytes();
        for (name, value) in headers {
            head.extend_from_slice(name.as_bytes());
            head.extend_from_slice(b": ");
            head.extend_from_slice(value.as_bytes());
            head.extend_from_slice(b"\r\n");
        }
        head.extend_from_slice(b"\r\n");
        let mut writes = vec![ResponseWrite::now(head)];
        for (delay, chunk) in chunks {
            let mut framed = format!("{:x}\r\n", chunk.len()).into_bytes();
            framed.extend_from_slice(&chunk);
            framed.extend_from_slice(b"\r\n");
            writes.push(ResponseWrite::after(delay, framed));
        }
        writes.push(ResponseWrite::now(b"0\r\n\r\n".to_vec()));
        Self::raw(writes)
    }
}

#[derive(Debug, Clone)]
pub struct ObservedRequest {
    pub request_line: String,
    headers: Vec<(String, String)>,
}

impl ObservedRequest {
    #[must_use]
    pub fn header_values(&self, name: &str) -> Vec<&str> {
        self.headers
            .iter()
            .filter_map(|(candidate, value)| {
                candidate
                    .eq_ignore_ascii_case(name)
                    .then_some(value.as_str())
            })
            .collect()
    }
}

#[derive(Debug, Default)]
struct FixtureState {
    plans: VecDeque<FictionalResponse>,
    observations: Vec<ObservedRequest>,
    stopping: bool,
}

/// Loopback-only fictional APS upstream controlled through in-process state.
///
/// There is deliberately no HTTP control route: the browser-facing request
/// cannot select a response plan or change the transport target.
pub struct ApsRunnerUpstream {
    address: SocketAddr,
    state: Arc<(Mutex<FixtureState>, Condvar)>,
    accept_thread: Option<thread::JoinHandle<()>>,
}

impl ApsRunnerUpstream {
    pub fn start() -> TestResult<Self> {
        let listener = TcpListener::bind("127.0.0.1:0")
            .change_context(TestError::RuntimeSpawn)
            .attach("failed to bind fictional APS runner upstream")?;
        let address = listener
            .local_addr()
            .change_context(TestError::RuntimeSpawn)?;
        let state = Arc::new((Mutex::new(FixtureState::default()), Condvar::new()));
        let server_state = Arc::clone(&state);
        let accept_thread = thread::spawn(move || {
            for incoming in listener.incoming() {
                let Ok(stream) = incoming else {
                    break;
                };
                let state = Arc::clone(&server_state);
                thread::spawn(move || serve_one(stream, &state));
                let stopping = server_state
                    .0
                    .lock()
                    .expect("fixture state should not be poisoned")
                    .stopping;
                if stopping {
                    break;
                }
            }
        });
        Ok(Self {
            address,
            state,
            accept_thread: Some(accept_thread),
        })
    }

    #[must_use]
    pub fn endpoint_url(&self) -> String {
        format!("http://{}/prebid-creative.js", self.address)
    }

    pub fn enqueue(&self, response: FictionalResponse) {
        let mut state = self
            .state
            .0
            .lock()
            .expect("fixture state should not be poisoned");
        state.plans.push_back(response);
    }

    pub fn wait_for_observation(&self, previous_count: usize) -> TestResult<ObservedRequest> {
        let deadline = Instant::now() + Duration::from_secs(2);
        let (lock, changed) = &*self.state;
        let mut state = lock.lock().expect("fixture state should not be poisoned");
        while proxy_observations(&state).count() <= previous_count {
            let now = Instant::now();
            if now >= deadline {
                return Err(Report::new(TestError::RuntimeNotReady)
                    .attach("fictional APS runner upstream did not observe the request"));
            }
            let result = changed
                .wait_timeout(state, deadline - now)
                .expect("fixture state should not be poisoned");
            state = result.0;
        }
        Ok(proxy_observations(&state)
            .nth(previous_count)
            .expect("proxy observation count was checked")
            .clone())
    }

    pub fn assert_no_proxy_observation(&self, previous_count: usize, duration: Duration) {
        let deadline = Instant::now() + duration;
        let (lock, changed) = &*self.state;
        let mut state = lock.lock().expect("fixture state should not be poisoned");
        while Instant::now() < deadline && proxy_observations(&state).count() <= previous_count {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            state = changed
                .wait_timeout(state, remaining)
                .expect("fixture state should not be poisoned")
                .0;
        }
        assert_eq!(
            proxy_observations(&state).count(),
            previous_count,
            "reserved non-GET request must not reach the APS upstream"
        );
    }
}

fn proxy_observations(state: &FixtureState) -> impl Iterator<Item = &ObservedRequest> {
    state
        .observations
        .iter()
        .filter(|request| !request.header_values("x-ts-aps-logical-url").is_empty())
}

impl Drop for ApsRunnerUpstream {
    fn drop(&mut self) {
        self.state
            .0
            .lock()
            .expect("fixture state should not be poisoned")
            .stopping = true;
        let _ = TcpStream::connect(self.address);
        if let Some(handle) = self.accept_thread.take() {
            let _ = handle.join();
        }
    }
}

fn serve_one(mut stream: TcpStream, state: &Arc<(Mutex<FixtureState>, Condvar)>) {
    let Ok(observation) = read_request(&mut stream) else {
        return;
    };
    let response = {
        let (lock, changed) = &**state;
        let mut state = lock.lock().expect("fixture state should not be poisoned");
        state.observations.push(observation);
        changed.notify_all();
        state.plans.pop_front()
    };
    let Some(response) = response else {
        let _ = stream.write_all(
            b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        );
        return;
    };
    for write in response.writes {
        if !write.delay_before.is_zero() {
            thread::sleep(write.delay_before);
        }
        if stream.write_all(&write.bytes).is_err() {
            break;
        }
        let _ = stream.flush();
    }
}

fn read_request(stream: &mut TcpStream) -> std::io::Result<ObservedRequest> {
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    let mut bytes = Vec::new();
    let mut chunk = [0_u8; 1024];
    while !bytes.windows(4).any(|window| window == b"\r\n\r\n") {
        let read = stream.read(&mut chunk)?;
        if read == 0 {
            break;
        }
        bytes.extend_from_slice(&chunk[..read]);
        if bytes.len() > MAX_REQUEST_HEAD_BYTES {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "request head exceeded fixture cap",
            ));
        }
    }
    let head = std::str::from_utf8(&bytes)
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidData, "non-UTF-8 request"))?;
    let mut lines = head.split("\r\n");
    let request_line = lines.next().unwrap_or_default().to_string();
    let headers = lines
        .take_while(|line| !line.is_empty())
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.trim().to_string(), value.trim().to_string()))
        .collect();
    Ok(ObservedRequest {
        request_line,
        headers,
    })
}
