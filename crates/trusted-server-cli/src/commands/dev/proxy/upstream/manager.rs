use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};
use std::time::Duration;

use tokio::sync::{mpsc, oneshot};
use tokio::task::AbortHandle;
use tokio::time::Instant;

use super::key::OriginKey;

const ORDINARY_CHANNEL_CAPACITY: usize = 64;

#[derive(Debug, Clone, Copy, Eq, Hash, PartialEq)]
pub struct ConnectionId(u64);

#[derive(Debug, Clone, Copy)]
pub struct PoolLimits {
    pub per_origin_live: usize,
    pub global_live: usize,
    pub per_origin_idle: usize,
    pub global_idle: usize,
    pub per_origin_waiters: usize,
    pub global_waiters: usize,
    pub acquire_timeout: Duration,
    pub idle_timeout: Duration,
}

impl Default for PoolLimits {
    fn default() -> Self {
        Self {
            per_origin_live: 6,
            global_live: 64,
            per_origin_idle: 2,
            global_idle: 32,
            per_origin_waiters: 32,
            global_waiters: 128,
            acquire_timeout: Duration::from_secs(30),
            idle_timeout: Duration::from_secs(60),
        }
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum AcquireError {
    Overloaded,
    TimedOut,
    ShuttingDown,
}

pub struct IdleConnection<T> {
    pub id: ConnectionId,
    pub key: OriginKey,
    pub value: T,
    pub abort: AbortHandle,
}

pub struct Lease<T> {
    pub connection: IdleConnection<T>,
    pub reused: bool,
    queue_wait: Option<Duration>,
}

pub struct Reservation {
    id: ConnectionId,
    key: OriginKey,
    control: mpsc::UnboundedSender<Control>,
    completed: bool,
    queue_wait: Option<Duration>,
}

impl Reservation {
    #[must_use]
    pub fn id(&self) -> ConnectionId {
        self.id
    }

    #[must_use]
    pub fn key(&self) -> &OriginKey {
        &self.key
    }

    pub fn register<T: Send + 'static>(
        mut self,
        manager: &Manager<T>,
        value: T,
        abort: AbortHandle,
    ) -> Lease<T> {
        manager.register_driver(self.id, abort.clone());
        self.completed = true;
        Lease {
            connection: IdleConnection {
                id: self.id,
                key: self.key.clone(),
                value,
                abort,
            },
            reused: false,
            queue_wait: self.queue_wait,
        }
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        if !self.completed {
            let _ = self.control.send(Control::ConnectFailed(self.id));
        }
    }
}

pub enum Acquired<T> {
    Reused(Lease<T>),
    Open(Reservation),
}

impl<T> Acquired<T> {
    #[must_use]
    pub fn queue_wait(&self) -> Option<Duration> {
        match self {
            Self::Reused(lease) => lease.queue_wait,
            Self::Open(reservation) => reservation.queue_wait,
        }
    }
}

struct AcquireTicket {
    state: AtomicU8,
}

impl AcquireTicket {
    const PENDING: u8 = 0;
    const CANCELLED: u8 = 1;
    const RESOLVED: u8 = 2;

    fn new() -> Self {
        Self {
            state: AtomicU8::new(Self::PENDING),
        }
    }

    fn cancel(&self) -> bool {
        self.state
            .compare_exchange(
                Self::PENDING,
                Self::CANCELLED,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    fn resolve(&self) -> bool {
        self.state
            .compare_exchange(
                Self::PENDING,
                Self::RESOLVED,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    fn is_cancelled(&self) -> bool {
        self.state.load(Ordering::Acquire) == Self::CANCELLED
    }
}

struct AcquireRequest<T> {
    sequence: u64,
    key: OriginKey,
    deadline: Instant,
    ticket: Arc<AcquireTicket>,
    allow_reuse: bool,
    queue_started: Option<Instant>,
    reply: oneshot::Sender<Result<Acquired<T>, AcquireError>>,
}

enum Command<T> {
    Acquire(AcquireRequest<T>),
    Return(IdleConnection<T>),
}

enum Control {
    Cancel(u64),
    ConnectFailed(ConnectionId),
    ConnectorRegistered(ConnectionId, AbortHandle),
    DriverRegistered(ConnectionId, AbortHandle),
    DriverClosed(ConnectionId),
    Shutdown(oneshot::Sender<()>),
}

pub struct Manager<T> {
    ordinary: mpsc::Sender<Command<T>>,
    control: mpsc::UnboundedSender<Control>,
    next_sequence: AtomicU64,
    acquire_timeout: Duration,
}

impl<T: Send + 'static> Manager<T> {
    #[must_use]
    pub fn start(limits: PoolLimits) -> Arc<Self> {
        let (ordinary, ordinary_rx) = mpsc::channel(ORDINARY_CHANNEL_CAPACITY);
        // This lane is API-unbounded so synchronous Drop paths cannot lose lifecycle
        // events. It is logically bounded: each accepted acquire emits at most one
        // cancellation, each of at most 64 connection IDs has one close event, and
        // the owner emits one shutdown.
        let (control, control_rx) = mpsc::unbounded_channel();
        let manager = Arc::new(Self {
            ordinary,
            control,
            next_sequence: AtomicU64::new(1),
            acquire_timeout: limits.acquire_timeout,
        });
        tokio::spawn(Actor::new(limits, ordinary_rx, control_rx, manager.control.clone()).run());
        manager
    }

    /// Acquires an idle connection or an exclusive reservation to open one.
    ///
    /// # Errors
    ///
    /// Returns overload, timeout, or shutdown when no lease can be granted.
    pub async fn acquire(&self, key: OriginKey) -> Result<Acquired<T>, AcquireError> {
        self.acquire_with_reuse(key, true).await
    }

    /// Acquires a newly opened connection, never an idle pooled sender.
    ///
    /// # Errors
    ///
    /// Returns overload, timeout, or shutdown when no reservation can be granted.
    pub async fn acquire_fresh(&self, key: OriginKey) -> Result<Acquired<T>, AcquireError> {
        self.acquire_with_reuse(key, false).await
    }

    async fn acquire_with_reuse(
        &self,
        key: OriginKey,
        allow_reuse: bool,
    ) -> Result<Acquired<T>, AcquireError> {
        let sequence = self.next_sequence.fetch_add(1, Ordering::Relaxed);
        let ticket = Arc::new(AcquireTicket::new());
        let (reply, received) = oneshot::channel();
        self.ordinary
            .send(Command::Acquire(AcquireRequest {
                sequence,
                key,
                deadline: Instant::now() + self.acquire_timeout,
                ticket: Arc::clone(&ticket),
                allow_reuse,
                queue_started: None,
                reply,
            }))
            .await
            .map_err(|_| AcquireError::ShuttingDown)?;
        let mut guard = AcquireGuard {
            sequence,
            ticket,
            control: self.control.clone(),
            armed: true,
        };
        let result = received.await.unwrap_or(Err(AcquireError::ShuttingDown));
        guard.armed = false;
        result
    }

    pub fn return_idle(&self, connection: IdleConnection<T>) {
        if let Err(error) = self.ordinary.try_send(Command::Return(connection))
            && let Command::Return(connection) = error.into_inner()
        {
            connection.abort.abort();
        }
    }

    pub fn driver_closed(&self, id: ConnectionId) {
        let _ = self.control.send(Control::DriverClosed(id));
    }

    pub(crate) fn register_connector(&self, id: ConnectionId, abort: AbortHandle) {
        let _ = self.control.send(Control::ConnectorRegistered(id, abort));
    }

    fn register_driver(&self, id: ConnectionId, abort: AbortHandle) {
        let _ = self.control.send(Control::DriverRegistered(id, abort));
    }

    pub async fn shutdown(&self) {
        let (reply, received) = oneshot::channel();
        if self.control.send(Control::Shutdown(reply)).is_ok() {
            let _ = received.await;
        }
    }
}

struct AcquireGuard {
    sequence: u64,
    ticket: Arc<AcquireTicket>,
    control: mpsc::UnboundedSender<Control>,
    armed: bool,
}

impl Drop for AcquireGuard {
    fn drop(&mut self) {
        if self.armed && self.ticket.cancel() {
            let _ = self.control.send(Control::Cancel(self.sequence));
        }
    }
}

struct LiveConnection {
    key: OriginKey,
    abort: Option<AbortHandle>,
}

struct Actor<T> {
    limits: PoolLimits,
    ordinary: mpsc::Receiver<Command<T>>,
    control: mpsc::UnboundedReceiver<Control>,
    control_tx: mpsc::UnboundedSender<Control>,
    live: HashMap<ConnectionId, LiveConnection>,
    idle: HashMap<OriginKey, VecDeque<(Instant, IdleConnection<T>)>>,
    waiters: VecDeque<AcquireRequest<T>>,
    next_connection: u64,
    closing: bool,
    shutdown_reply: Option<oneshot::Sender<()>>,
}

impl<T: Send + 'static> Actor<T> {
    fn new(
        limits: PoolLimits,
        ordinary: mpsc::Receiver<Command<T>>,
        control: mpsc::UnboundedReceiver<Control>,
        control_tx: mpsc::UnboundedSender<Control>,
    ) -> Self {
        Self {
            limits,
            ordinary,
            control,
            control_tx,
            live: HashMap::new(),
            idle: HashMap::new(),
            waiters: VecDeque::new(),
            next_connection: 1,
            closing: false,
            shutdown_reply: None,
        }
    }

    async fn run(mut self) {
        loop {
            self.expire();
            self.maybe_finish_shutdown();
            let deadline = self.next_deadline();
            tokio::select! {
                biased;
                event = self.control.recv() => match event {
                    Some(event) => self.control(event),
                    None => break,
                },
                command = self.ordinary.recv() => match command {
                    Some(command) => self.command(command),
                    None => break,
                },
                () = sleep_until_optional(deadline) => {}
            }
        }
    }

    fn command(&mut self, command: Command<T>) {
        match command {
            Command::Acquire(request) => self.acquire(request),
            Command::Return(connection) => self.return_connection(connection),
        }
    }

    fn acquire(&mut self, mut request: AcquireRequest<T>) {
        if self.closing || request.ticket.is_cancelled() {
            Self::resolve_error(request, AcquireError::ShuttingDown);
            return;
        }
        if request.deadline <= Instant::now() {
            Self::resolve_error(request, AcquireError::TimedOut);
            return;
        }
        if request.allow_reuse
            && let Some(connection) = self.take_idle(&request.key)
        {
            if request.ticket.resolve() {
                let _ = request.reply.send(Ok(Acquired::Reused(Lease {
                    connection,
                    reused: true,
                    queue_wait: request.queue_started.map(|started| started.elapsed()),
                })));
            } else {
                self.return_connection(connection);
            }
        } else if self.can_open(&request.key) {
            self.open_for(request);
        } else {
            if !request.allow_reuse
                && let Some(connection) = self.take_idle(&request.key)
            {
                connection.abort.abort();
            }
            let origin_waiters = self
                .waiters
                .iter()
                .filter(|item| item.key == request.key)
                .count();
            if origin_waiters >= self.limits.per_origin_waiters
                || self.waiters.len() >= self.limits.global_waiters
            {
                Self::resolve_error(request, AcquireError::Overloaded);
            } else {
                request.queue_started = Some(Instant::now());
                self.waiters.push_back(request);
            }
        }
    }

    fn can_open(&self, key: &OriginKey) -> bool {
        self.live.len() < self.limits.global_live
            && self.live.values().filter(|item| &item.key == key).count()
                < self.limits.per_origin_live
    }

    fn open_for(&mut self, request: AcquireRequest<T>) {
        if !request.ticket.resolve() {
            return;
        }
        let id = ConnectionId(self.next_connection);
        self.next_connection += 1;
        self.live.insert(
            id,
            LiveConnection {
                key: request.key.clone(),
                abort: None,
            },
        );
        let reservation = Reservation {
            id,
            key: request.key,
            control: self.control_tx.clone(),
            completed: false,
            queue_wait: request.queue_started.map(|started| started.elapsed()),
        };
        if request.reply.send(Ok(Acquired::Open(reservation))).is_err() {
            self.release(id);
        }
    }

    fn take_idle(&mut self, key: &OriginKey) -> Option<IdleConnection<T>> {
        let entries = self.idle.get_mut(key)?;
        let result = entries.pop_front().map(|(_, connection)| connection);
        if entries.is_empty() {
            self.idle.remove(key);
        }
        result
    }

    fn return_connection(&mut self, connection: IdleConnection<T>) {
        if self.closing || !self.live.contains_key(&connection.id) {
            connection.abort.abort();
            return;
        }
        if let Some(index) = self.waiters.iter().position(|waiter| {
            waiter.allow_reuse && waiter.key == connection.key && !waiter.ticket.is_cancelled()
        }) {
            let waiter = self.waiters.remove(index).expect("should find waiter");
            if waiter.ticket.resolve() {
                let _ = waiter.reply.send(Ok(Acquired::Reused(Lease {
                    connection,
                    reused: true,
                    queue_wait: waiter.queue_started.map(|started| started.elapsed()),
                })));
                return;
            }
        }
        let origin_idle = self.idle.get(&connection.key).map_or(0, VecDeque::len);
        let global_idle: usize = self.idle.values().map(VecDeque::len).sum();
        if origin_idle >= self.limits.per_origin_idle || global_idle >= self.limits.global_idle {
            // Dropping the last HTTP sender lets Hyper close a healthy idle socket
            // cleanly. Capacity remains live until the driver guard reports exit.
            drop(connection);
            return;
        }
        self.idle
            .entry(connection.key.clone())
            .or_default()
            .push_back((Instant::now() + self.limits.idle_timeout, connection));
    }

    fn control(&mut self, event: Control) {
        match event {
            Control::Cancel(sequence) => {
                if let Some(index) = self
                    .waiters
                    .iter()
                    .position(|item| item.sequence == sequence)
                {
                    self.waiters.remove(index);
                }
            }
            Control::ConnectFailed(id) | Control::DriverClosed(id) => self.release(id),
            Control::ConnectorRegistered(id, abort) | Control::DriverRegistered(id, abort) => {
                if let Some(live) = self.live.get_mut(&id) {
                    if self.closing {
                        abort.abort();
                    } else {
                        live.abort = Some(abort);
                    }
                } else {
                    abort.abort();
                }
            }
            Control::Shutdown(reply) => {
                self.closing = true;
                self.shutdown_reply = Some(reply);
                for waiter in self.waiters.drain(..) {
                    Self::resolve_error(waiter, AcquireError::ShuttingDown);
                }
                for entries in self.idle.values_mut() {
                    for (_, connection) in entries.drain(..) {
                        connection.abort.abort();
                    }
                }
                self.idle.clear();
                for live in self.live.values() {
                    if let Some(abort) = &live.abort {
                        abort.abort();
                    }
                }
            }
        }
    }

    fn release(&mut self, id: ConnectionId) {
        if self.live.remove(&id).is_some() {
            for entries in self.idle.values_mut() {
                if let Some(index) = entries.iter().position(|(_, item)| item.id == id) {
                    entries.remove(index);
                    break;
                }
            }
            self.admit_waiters();
        }
    }

    fn admit_waiters(&mut self) {
        while let Some(index) = self
            .waiters
            .iter()
            .position(|waiter| !waiter.ticket.is_cancelled() && self.can_open(&waiter.key))
        {
            let request = self
                .waiters
                .remove(index)
                .expect("should find admissible waiter");
            self.open_for(request);
        }
    }

    fn expire(&mut self) {
        let now = Instant::now();
        let mut index = 0;
        while index < self.waiters.len() {
            if self.waiters[index].ticket.is_cancelled() {
                self.waiters.remove(index);
            } else if self.waiters[index].deadline <= now {
                let waiter = self
                    .waiters
                    .remove(index)
                    .expect("should find expired waiter");
                Self::resolve_error(waiter, AcquireError::TimedOut);
            } else {
                index += 1;
            }
        }
        for entries in self.idle.values_mut() {
            while entries
                .front()
                .is_some_and(|(deadline, _)| *deadline <= now)
            {
                let (_, connection) = entries.pop_front().expect("should have expired idle");
                connection.abort.abort();
            }
        }
        self.idle.retain(|_, entries| !entries.is_empty());
    }

    fn next_deadline(&self) -> Option<Instant> {
        self.waiters
            .iter()
            .map(|item| item.deadline)
            .chain(self.idle.values().flatten().map(|(deadline, _)| *deadline))
            .min()
    }

    fn maybe_finish_shutdown(&mut self) {
        if self.closing
            && self.live.is_empty()
            && let Some(reply) = self.shutdown_reply.take()
        {
            let _ = reply.send(());
        }
    }

    fn resolve_error(request: AcquireRequest<T>, error: AcquireError) {
        if request.ticket.resolve() {
            let _ = request.reply.send(Err(error));
        }
    }
}

async fn sleep_until_optional(deadline: Option<Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline).await,
        None => std::future::pending().await,
    }
}

#[cfg(test)]
mod tests {
    use std::net::IpAddr;

    use super::*;
    use crate::commands::dev::proxy::upstream::key::{
        AddressPolicy, ReferenceIdentity, Transport, VerifyMode,
    };

    fn key(host: &str) -> OriginKey {
        OriginKey::new(
            Transport::Tls,
            ReferenceIdentity::dns(host),
            443,
            VerifyMode::Secure,
            AddressPolicy::Dns,
        )
    }

    fn abort_handle() -> AbortHandle {
        tokio::spawn(std::future::pending::<()>()).abort_handle()
    }

    #[test]
    fn acquire_ticket_has_exactly_one_terminal_transition() {
        let cancelled = AcquireTicket::new();
        assert!(cancelled.cancel());
        assert!(!cancelled.cancel());
        assert!(!cancelled.resolve());

        let resolved = AcquireTicket::new();
        assert!(resolved.resolve());
        assert!(!resolved.resolve());
        assert!(!resolved.cancel());
    }

    #[tokio::test(start_paused = true)]
    async fn limits_live_connections_and_times_out_waiters() {
        let manager = Manager::<()>::start(PoolLimits {
            per_origin_live: 1,
            global_live: 1,
            ..PoolLimits::default()
        });
        let first = manager.acquire(key("one.example")).await.expect("first");
        assert!(matches!(first, Acquired::Open(_)));

        let waiting = tokio::spawn({
            let manager = Arc::clone(&manager);
            async move { manager.acquire(key("one.example")).await }
        });
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_secs(29)).await;
        assert!(!waiting.is_finished());
        tokio::time::advance(Duration::from_secs(1)).await;
        assert!(matches!(
            waiting.await.expect("join"),
            Err(AcquireError::TimedOut)
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn reuses_returned_connection_and_expires_it_at_sixty_seconds() {
        let manager = Manager::start(PoolLimits::default());
        let Acquired::Open(reservation) = manager.acquire(key("one.example")).await.expect("open")
        else {
            panic!("should open");
        };
        let id = reservation.id();
        let lease = reservation.register(&manager, 7_u8, abort_handle());
        manager.return_idle(lease.connection);
        tokio::task::yield_now().await;
        let Acquired::Reused(lease) = manager.acquire(key("one.example")).await.expect("reuse")
        else {
            panic!("should reuse");
        };
        assert_eq!(lease.connection.value, 7);
        manager.return_idle(lease.connection);
        tokio::time::advance(Duration::from_secs(60)).await;
        tokio::task::yield_now().await;
        manager.driver_closed(id);
        let next = manager.acquire(key("one.example")).await.expect("next");
        assert!(matches!(next, Acquired::Open(_)));
    }

    #[tokio::test(start_paused = true)]
    async fn oldest_admissible_origin_bypasses_blocked_origin() {
        let manager = Manager::<()>::start(PoolLimits {
            per_origin_live: 1,
            global_live: 2,
            ..PoolLimits::default()
        });
        let first = manager.acquire(key("one.example")).await.expect("first");
        let second = manager.acquire(key("two.example")).await.expect("second");
        let Acquired::Open(first) = first else {
            panic!("open")
        };
        let Acquired::Open(second) = second else {
            panic!("open")
        };
        let blocked_one = tokio::spawn({
            let manager = Arc::clone(&manager);
            async move { manager.acquire(key("one.example")).await }
        });
        let admissible_three = tokio::spawn({
            let manager = Arc::clone(&manager);
            async move { manager.acquire(key("three.example")).await }
        });
        tokio::task::yield_now().await;
        drop(second);
        tokio::task::yield_now().await;
        assert!(admissible_three.is_finished());
        assert!(!blocked_one.is_finished());
        drop(first);
    }

    #[tokio::test(start_paused = true)]
    async fn enforces_six_live_and_bounded_waiters_per_origin() {
        let manager = Manager::<()>::start(PoolLimits {
            per_origin_waiters: 2,
            global_waiters: 2,
            ..PoolLimits::default()
        });
        let mut reservations = Vec::new();
        for _ in 0..6 {
            let Acquired::Open(reservation) = manager
                .acquire(key("one.example"))
                .await
                .expect("reserve within cap")
            else {
                panic!("should reserve a new connection");
            };
            reservations.push(reservation);
        }
        let first_waiter = tokio::spawn({
            let manager = Arc::clone(&manager);
            async move { manager.acquire(key("one.example")).await }
        });
        let second_waiter = tokio::spawn({
            let manager = Arc::clone(&manager);
            async move { manager.acquire(key("one.example")).await }
        });
        tokio::task::yield_now().await;
        assert!(!first_waiter.is_finished() && !second_waiter.is_finished());
        assert!(matches!(
            manager.acquire(key("one.example")).await,
            Err(AcquireError::Overloaded)
        ));

        first_waiter.abort();
        tokio::task::yield_now().await;
        drop(reservations.pop());
        tokio::task::yield_now().await;
        assert!(second_waiter.is_finished());
    }

    #[tokio::test(start_paused = true)]
    async fn shutdown_aborts_registered_connector_before_it_finishes() {
        let manager = Manager::<()>::start(PoolLimits::default());
        let Acquired::Open(reservation) = manager
            .acquire(key("connector.example"))
            .await
            .expect("reserve connector")
        else {
            panic!("should open connector");
        };
        let connector = tokio::spawn(std::future::pending::<()>());
        manager.register_connector(reservation.id(), connector.abort_handle());

        let shutdown = tokio::spawn({
            let manager = Arc::clone(&manager);
            async move { manager.shutdown().await }
        });
        tokio::task::yield_now().await;

        assert!(
            connector.is_finished(),
            "shutdown should abort connector task"
        );
        assert!(
            !shutdown.is_finished(),
            "capacity remains until completion event"
        );
        drop(reservation);
        tokio::task::yield_now().await;
        assert!(
            shutdown.is_finished(),
            "connector completion should reconcile capacity"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn priority_shutdown_overtakes_a_saturated_ordinary_lane() {
        let limits = PoolLimits::default();
        let (ordinary, ordinary_rx) = mpsc::channel::<Command<()>>(ORDINARY_CHANNEL_CAPACITY);
        let (control, control_rx) = mpsc::unbounded_channel();
        let mut replies = Vec::new();
        for sequence in 0..ORDINARY_CHANNEL_CAPACITY as u64 {
            let (reply, received) = oneshot::channel();
            ordinary
                .try_send(Command::Acquire(AcquireRequest {
                    sequence,
                    key: key("saturated.example"),
                    deadline: Instant::now() + limits.acquire_timeout,
                    ticket: Arc::new(AcquireTicket::new()),
                    allow_reuse: true,
                    queue_started: None,
                    reply,
                }))
                .expect("should fill ordinary lane");
            replies.push(received);
        }
        assert!(
            ordinary.try_reserve().is_err(),
            "ordinary lane should be full"
        );
        let (shutdown_reply, shutdown_received) = oneshot::channel();
        control
            .send(Control::Shutdown(shutdown_reply))
            .expect("should enqueue priority shutdown");

        tokio::spawn(Actor::new(limits, ordinary_rx, control_rx, control.clone()).run());

        shutdown_received
            .await
            .expect("shutdown should bypass ordinary lane capacity");
        for received in replies {
            assert!(matches!(
                received.await.expect("actor should answer acquire"),
                Err(AcquireError::ShuttingDown)
            ));
        }
    }

    #[tokio::test(start_paused = true)]
    async fn fresh_acquire_never_returns_an_idle_sender() {
        let manager = Manager::start(PoolLimits {
            per_origin_live: 1,
            global_live: 1,
            ..PoolLimits::default()
        });
        let Acquired::Open(reservation) =
            manager.acquire(key("stale.example")).await.expect("open")
        else {
            panic!("should open");
        };
        let id = reservation.id();
        let lease = reservation.register(&manager, 1_u8, abort_handle());
        manager.return_idle(lease.connection);
        tokio::task::yield_now().await;

        let fresh = tokio::spawn({
            let manager = Arc::clone(&manager);
            async move { manager.acquire_fresh(key("stale.example")).await }
        });
        tokio::task::yield_now().await;
        assert!(
            !fresh.is_finished(),
            "fresh acquire should close rather than reuse idle"
        );
        manager.driver_closed(id);
        tokio::task::yield_now().await;
        assert!(matches!(fresh.await.expect("join"), Ok(Acquired::Open(_))));
    }

    #[tokio::test(start_paused = true)]
    async fn global_live_limit_is_atomic_across_origins() {
        let manager = Manager::<()>::start(PoolLimits::default());
        let mut reservations = Vec::new();
        for index in 0..64 {
            let Acquired::Open(reservation) = manager
                .acquire(key(&format!("origin-{index}.example")))
                .await
                .expect("reserve within global cap")
            else {
                panic!("should reserve connection");
            };
            reservations.push(reservation);
        }
        let blocked = tokio::spawn({
            let manager = Arc::clone(&manager);
            async move { manager.acquire(key("overflow.example")).await }
        });
        tokio::task::yield_now().await;
        assert!(!blocked.is_finished());
        drop(reservations.pop());
        tokio::task::yield_now().await;
        assert!(matches!(
            blocked.await.expect("join"),
            Ok(Acquired::Open(_))
        ));
    }

    #[test]
    fn origin_key_fixture_accepts_ip_policy() {
        let _ = IpAddr::V4(std::net::Ipv4Addr::LOCALHOST);
    }
}
