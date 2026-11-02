//! Shared ownership and deadline collection for dispatched runtime operations.

/// Receives a dispatched operation and cancels it when its owner is dropped.
pub struct PlatformPendingResponse<T> {
    response: futures::channel::oneshot::Receiver<T>,
    abort: futures::future::AbortHandle,
}

impl<T: 'static> PlatformPendingResponse<T> {
    /// Starts an operation with the caller's runtime spawner.
    ///
    /// The operation publishes only its complete result. Dropping this owner
    /// aborts queued or running guest work when the executor next polls it.
    /// Runtime-specific transport cancellation remains the caller's responsibility.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use futures::task::LocalSpawnExt as _;
    /// use trusted_server_core::platform::PlatformPendingResponse;
    /// let pool = futures::executor::LocalPool::new();
    /// let pending = PlatformPendingResponse::start(async { 42 }, |task| {
    ///     pool.spawner().spawn_local(task).expect("should spawn operation");
    /// });
    /// ```
    pub fn start(
        operation: impl std::future::Future<Output = T> + 'static,
        spawn: impl FnOnce(futures::future::LocalBoxFuture<'static, ()>),
    ) -> Self {
        let (sender, response) = futures::channel::oneshot::channel();
        let (abort, registration) = futures::future::AbortHandle::new_pair();
        spawn(Box::pin(async move {
            if let Ok(result) = futures::future::Abortable::new(operation, registration).await {
                let _ = sender.send(result);
            }
        }));
        Self { response, abort }
    }

    /// Waits for the complete result while retaining cancellation ownership.
    ///
    /// # Errors
    ///
    /// Returns a channel error if the dispatched operation stops without a result.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use trusted_server_core::platform::PlatformPendingResponse;
    /// # async fn collect(pending: &mut PlatformPendingResponse<u32>) -> Result<u32, futures::channel::oneshot::Canceled> {
    /// let result = pending.wait().await?;
    /// # Ok(result)
    /// # }
    /// ```
    pub async fn wait(&mut self) -> Result<T, futures::channel::oneshot::Canceled> {
        (&mut self.response).await
    }

    /// Collects a buffered result, waiting at most until the supplied timer fires.
    ///
    /// An initial nonblocking probe retains results after an expired deadline.
    /// A final probe handles a result published in the same turn as timer expiry.
    /// The caller supplies the runtime timer and a monotonic clock; no transport
    /// is polled directly here. Returns `None` when the deadline expires without
    /// a complete buffered result. The owner remains alive throughout the wait.
    ///
    /// # Errors
    ///
    /// Returns a channel error inside `Some` if the operation stops without a result.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use trusted_server_core::platform::PlatformPendingResponse;
    /// # async fn collect(pending: &mut PlatformPendingResponse<u32>, timer: impl std::future::Future<Output = ()>, deadline: web_time::Instant) {
    /// let result = pending.complete_before_deadline(timer, deadline, web_time::Instant::now).await;
    /// # }
    /// ```
    pub async fn complete_before_deadline<F, N>(
        &mut self,
        timer: F,
        deadline: web_time::Instant,
        now: N,
    ) -> Option<Result<T, futures::channel::oneshot::Canceled>>
    where
        F: std::future::Future<Output = ()>,
        N: Fn() -> web_time::Instant,
    {
        complete_before_deadline(&mut self.response, timer, deadline, now).await
    }
}

impl<T> Drop for PlatformPendingResponse<T> {
    fn drop(&mut self) {
        self.abort.abort();
    }
}

async fn complete_before_deadline<R, T, N>(
    response: &mut futures::channel::oneshot::Receiver<R>,
    timer: T,
    deadline: web_time::Instant,
    now: N,
) -> Option<Result<R, futures::channel::oneshot::Canceled>>
where
    T: std::future::Future<Output = ()>,
    N: Fn() -> web_time::Instant,
{
    // Inspect only the task's buffered result, including when collection starts
    // after publisher-origin work has exhausted the auction budget.
    match response.try_recv() {
        Ok(Some(ready)) => return Some(Ok(ready)),
        Err(error) => return Some(Err(error)),
        Ok(None) => {}
    }
    if now() >= deadline {
        return None;
    }
    match futures::future::select(response, Box::pin(timer)).await {
        futures::future::Either::Left((ready, _)) => Some(ready),
        // The timer and task can become ready in the same executor turn. A final
        // channel probe retains buffered results without polling transport I/O.
        futures::future::Either::Right(((), response)) => response.try_recv().transpose(),
    }
}

#[cfg(test)]
mod deadline_tests {
    use std::cell::Cell;
    use std::rc::Rc;
    use std::time::Duration;

    use futures::executor::{LocalPool, block_on};
    use futures::future::{pending, ready};
    use futures::task::LocalSpawnExt as _;
    use web_time::Instant;

    use super::{PlatformPendingResponse, complete_before_deadline};

    #[test]
    fn dispatched_task_starts_before_collection_and_buffers_complete_body() {
        let mut executor = LocalPool::new();
        let spawner = executor.spawner();
        let started = Rc::new(Cell::new(false));
        let started_operation = Rc::clone(&started);
        let (finish_body, body) = futures::channel::oneshot::channel();
        let mut pending = PlatformPendingResponse::start(
            async move {
                started_operation.set(true);
                body.await.expect("should receive complete response body")
            },
            |task| {
                spawner
                    .spawn_local(task)
                    .expect("should spawn outbound task")
            },
        );

        executor.run_until_stalled();
        assert!(started.get(), "should start outbound work before selection");
        assert_eq!(
            pending.response.try_recv().expect("should poll result"),
            None,
            "should not expose headers before response body completes"
        );
        finish_body.send(42).expect("should finish response body");
        executor.run_until_stalled();
        assert_eq!(
            pending
                .response
                .try_recv()
                .expect("should poll buffered result"),
            Some(42),
            "should preserve a complete buffered response without polling transport"
        );
    }

    #[test]
    fn dropping_dispatched_pending_cancels_started_body_operation() {
        struct DropProbe(Rc<Cell<bool>>);
        impl Drop for DropProbe {
            fn drop(&mut self) {
                self.0.set(true);
            }
        }
        let mut executor = LocalPool::new();
        let spawner = executor.spawner();
        let dropped = Rc::new(Cell::new(false));
        let probe = DropProbe(Rc::clone(&dropped));
        let pending = PlatformPendingResponse::start(
            async move {
                let _probe = probe;
                futures::future::pending::<()>().await;
            },
            |task| {
                spawner
                    .spawn_local(task)
                    .expect("should spawn outbound task")
            },
        );
        executor.run_until_stalled();
        assert!(!dropped.get(), "should retain an active body operation");
        drop(pending);
        executor.run_until_stalled();
        assert!(
            dropped.get(),
            "should drop spawned operation after cancellation"
        );
    }

    #[test]
    fn dropping_queued_dispatch_prevents_transport_poll() {
        let mut executor = LocalPool::new();
        let spawner = executor.spawner();
        let started = Rc::new(Cell::new(false));
        let operation_started = Rc::clone(&started);
        let pending = PlatformPendingResponse::start(
            async move {
                operation_started.set(true);
            },
            |task| {
                spawner
                    .spawn_local(task)
                    .expect("should spawn outbound task")
            },
        );
        drop(pending);
        executor.run_until_stalled();
        assert!(
            !started.get(),
            "should cancel queued work before transport starts"
        );
    }

    #[test]
    fn expired_deadline_preserves_buffered_response() {
        let (sender, mut response) = futures::channel::oneshot::channel();
        sender.send(42).expect("should buffer response");
        let deadline = Instant::now();
        let result = block_on(complete_before_deadline(
            &mut response,
            pending(),
            deadline,
            || deadline,
        ));
        assert_eq!(
            result,
            Some(Ok(42)),
            "should retain a buffered response after cutoff"
        );
    }

    #[test]
    fn expired_deadline_returns_without_waiting_for_response() {
        let (_sender, mut response) = futures::channel::oneshot::channel::<()>();
        let deadline = Instant::now();
        let result = block_on(complete_before_deadline(
            &mut response,
            pending(),
            deadline,
            || deadline,
        ));
        assert_eq!(result, None, "should stop waiting at an expired deadline");
    }

    #[test]
    fn timer_cuts_off_pending_response() {
        let (_sender, mut response) = futures::channel::oneshot::channel::<()>();
        let now = Instant::now();
        let result = block_on(complete_before_deadline(
            &mut response,
            ready(()),
            now + Duration::from_secs(1),
            || now,
        ));
        assert_eq!(result, None, "should cut off an unbuffered response");
    }

    #[test]
    fn closed_response_channel_reports_failure() {
        let (sender, mut response) = futures::channel::oneshot::channel::<()>();
        drop(sender);
        let now = Instant::now();
        let result = block_on(complete_before_deadline(
            &mut response,
            pending(),
            now + Duration::from_secs(1),
            || now,
        ));
        assert!(
            matches!(result, Some(Err(_))),
            "should preserve a dispatched task failure"
        );
    }

    #[test]
    fn timer_rechecks_result_published_during_cutoff() {
        let (sender, mut response) = futures::channel::oneshot::channel();
        let now = Instant::now();
        let clock = Cell::new(now);
        let deadline = now + Duration::from_secs(1);
        let timer = async {
            clock.set(deadline);
            sender.send(42).expect("should publish at cutoff");
        };
        let result = block_on(complete_before_deadline(
            &mut response,
            timer,
            deadline,
            || clock.get(),
        ));
        assert_eq!(
            result,
            Some(Ok(42)),
            "should retain result buffered when timer wins"
        );
        assert_eq!(
            clock.get(),
            deadline,
            "should exercise the deadline boundary"
        );
    }
}
