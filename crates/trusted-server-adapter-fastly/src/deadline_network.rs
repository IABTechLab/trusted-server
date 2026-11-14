//! Auction deadline cases for `EdgeZero`'s Fastly client against a slow origin.
//!
//! `tests/deadline-origin.mjs` starts a local origin, writes its port into the
//! inline `deadline_fixture` config store, and runs the ignored `network_`
//! cases under Viceroy. Each case drives a batch from
//! [`HttpClient::start_batch_until`](edgezero_core::outbound::HttpClient::start_batch_until)
//! with no request lifecycle, so the batch gets a caller-owned scope and every
//! wait reaches the host select.

mod tests {
    use std::time::{Duration, Instant};

    use edgezero_adapter_fastly::outbound::FastlyOutboundClient;
    use edgezero_core::http::{Method, StatusCode};
    use edgezero_core::outbound::{
        HttpClient, OutboundBatchNext, OutboundBatchTermination, OutboundCachePolicy,
        OutboundCollectedSlot, OutboundRequest, ResponseDecoding,
    };
    use edgezero_core::time::Deadline;
    use futures::executor::block_on;
    use trusted_server_core::outbound::{
        BUFFERED_TIMEOUT, DEFAULT_BUFFERED_RESPONSE_BYTES, buffered, outbound_request,
    };

    fn client() -> HttpClient {
        HttpClient::with_client(FastlyOutboundClient::new())
    }

    /// Builds a buffered, decoded request to `path` on the harness origin.
    ///
    /// It bypasses the cache and uses the default 10 MiB response cap. The
    /// cases test the batch's waits, not a bidder's response cap.
    fn fixture_request(method: Method, path: &str) -> OutboundRequest {
        let port = fastly::ConfigStore::open("deadline_fixture")
            .get("port")
            .expect("should read the origin port the harness wrote");
        let target = outbound_request(method, &format!("http://127.0.0.1:{port}/{path}"))
            .expect("should build the fixture request");
        buffered(
            target,
            BUFFERED_TIMEOUT,
            DEFAULT_BUFFERED_RESPONSE_BYTES,
            ResponseDecoding::Decode,
        )
        .cache_policy(OutboundCachePolicy::Bypass)
    }

    #[test]
    #[ignore = "requires the local origin started by tests/deadline-origin.mjs"]
    fn network_stalled_headers_end_at_cutoff() {
        let request = fixture_request(Method::GET, "stalled");
        let started = Instant::now();
        let batch =
            client().start_batch_until(vec![request], Deadline::after(Duration::from_millis(80)));
        let (slots, termination) = block_on(batch.collect())
            .expect("should collect the stalled batch")
            .into_parts();

        assert_eq!(
            termination,
            OutboundBatchTermination::Cutoff,
            "should end at the cutoff while headers stay pending"
        );
        assert!(
            matches!(slots.as_slice(), [OutboundCollectedSlot::Unresolved]),
            "should leave the stalled slot unresolved, got {slots:?}"
        );
        assert!(
            started.elapsed() < Duration::from_millis(500),
            "should bound the header wait at the cutoff"
        );
    }

    #[test]
    #[ignore = "requires the local origin started by tests/deadline-origin.mjs"]
    fn network_trickling_body_ends_at_cutoff() {
        let request = fixture_request(Method::GET, "trickle");
        let started = Instant::now();
        let batch =
            client().start_batch_until(vec![request], Deadline::after(Duration::from_millis(80)));
        let (slots, termination) = block_on(batch.collect())
            .expect("should collect the trickling batch")
            .into_parts();

        assert_eq!(
            termination,
            OutboundBatchTermination::Cutoff,
            "should end at the cutoff before the trickling body completes"
        );
        assert!(
            matches!(slots.as_slice(), [OutboundCollectedSlot::Unresolved]),
            "should leave the trickling slot unresolved, got {slots:?}"
        );
        assert!(
            started.elapsed() < Duration::from_millis(500),
            "should bound body reads at the cutoff"
        );
    }

    #[test]
    #[ignore = "requires the local origin started by tests/deadline-origin.mjs"]
    fn network_head_skips_declared_length() {
        let request = fixture_request(Method::HEAD, "head");
        let batch =
            client().start_batch_until(vec![request], Deadline::after(Duration::from_secs(1)));
        let (slots, termination) = block_on(batch.collect())
            .expect("should collect the HEAD batch")
            .into_parts();

        assert_eq!(
            termination,
            OutboundBatchTermination::Completed,
            "should complete before the cutoff"
        );
        let [OutboundCollectedSlot::Resolved(head)] = slots.as_slice() else {
            panic!("should resolve the HEAD slot, got {slots:?}");
        };
        let response = head
            .outcome
            .as_ref()
            .expect("should accept a HEAD whose declared length exceeds the response cap");
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "should keep the HEAD status"
        );
    }

    #[test]
    #[ignore = "requires the local origin started by tests/deadline-origin.mjs"]
    fn network_fast_sibling_completes() {
        let requests = vec![
            fixture_request(Method::GET, "trickle"),
            fixture_request(Method::GET, "fast"),
        ];
        let batch =
            client().start_batch_until(requests, Deadline::after(Duration::from_millis(200)));
        let (slots, termination) = block_on(batch.collect())
            .expect("should collect the mixed batch")
            .into_parts();

        assert_eq!(
            termination,
            OutboundBatchTermination::Cutoff,
            "should end at the cutoff while the trickling body is incomplete"
        );
        let [
            OutboundCollectedSlot::Unresolved,
            OutboundCollectedSlot::Resolved(fast),
        ] = slots.as_slice()
        else {
            panic!("should resolve only the fast sibling, got {slots:?}");
        };
        let response = fast
            .outcome
            .as_ref()
            .expect("should collect the fast sibling");
        assert_eq!(
            response.body().as_bytes(),
            Some(b"fast".as_slice()),
            "should not let a trickling sibling starve the fast reply"
        );
    }

    #[test]
    #[ignore = "requires the local origin started by tests/deadline-origin.mjs"]
    fn network_expired_snapshot_drops_reply_landed_during_sleep() {
        let requests = vec![
            fixture_request(Method::GET, "fast"),
            fixture_request(Method::GET, "stalled"),
        ];
        let mut batch =
            client().start_batch_until(requests, Deadline::after(Duration::from_millis(40)));
        // The fast reply lands about 30 ms in, while the guest sleeps past the
        // cutoff without driving the batch.
        std::thread::sleep(Duration::from_millis(100));
        let woke = Instant::now();
        let next = block_on(batch.next()).expect("should observe the batch end");

        assert!(
            matches!(
                next,
                OutboundBatchNext::Finished(OutboundBatchTermination::Cutoff)
            ),
            "should drop the reply that landed during the sleep, got {next:?}"
        );
        assert!(
            woke.elapsed() < Duration::from_millis(100),
            "should end at the expired cutoff without a host wait"
        );
    }

    #[test]
    #[ignore = "requires the local origin started by tests/deadline-origin.mjs"]
    fn network_reply_observed_during_wait_before_cutoff_is_kept() {
        let requests = vec![
            fixture_request(Method::GET, "stalled"),
            fixture_request(Method::GET, "fast"),
        ];
        let started = Instant::now();
        let mut batch =
            client().start_batch_until(requests, Deadline::after(Duration::from_millis(300)));
        let first = block_on(batch.next()).expect("should observe the fast reply");
        let observed = started.elapsed();
        let last = block_on(batch.next()).expect("should observe the batch end");

        let OutboundBatchNext::Item(item) = first else {
            panic!("should keep the reply observed before the cutoff, got {first:?}");
        };
        let (index, result) = item.into_parts();
        assert_eq!(index, 1, "should deliver the fast slot");
        let response = result.outcome.expect("should answer the fast slot");
        assert_eq!(
            response.body().as_bytes(),
            Some(b"fast".as_slice()),
            "should keep the fast reply body"
        );
        assert!(
            observed < Duration::from_millis(300),
            "should deliver the reply before the cutoff, took {observed:?}"
        );
        assert!(
            matches!(
                last,
                OutboundBatchNext::Finished(OutboundBatchTermination::Cutoff)
            ),
            "should end at the cutoff while the stalled sibling waits, got {last:?}"
        );
        assert!(
            started.elapsed() < Duration::from_millis(600),
            "should bound the wait for the stalled sibling at the cutoff"
        );
    }
}
