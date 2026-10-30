/** An explicitly browser-carried fixture; this does not prove live auction capture. */
export function storedTraceReportFixture(origin: string, now = Date.now()) {
  const capturedAt = new Date(now).toISOString()
  const absent = { source: 'request', state: 'absent' }
  const counts = { observed: 0, matched: 0, unmatched: 0, ambiguous: 0 }
  return {
    stored_at_ms: now,
    report: {
      schema_version: 1,
      captured_at: capturedAt,
      request_context: {
        schema_version: 1,
        captured_at: capturedAt,
        network: {
          tls_cipher: '<img src=x onerror=alert(1)>',
          edge_hostname: `${'x'.repeat(116)}.example.com`,
        },
        cookies: {
          ts_ec: absent,
          ts_eids: absent,
          ts_tester: absent,
          diagnostics_session: {
            source: 'request',
            state: 'unavailable',
            detail: 'runtime_header_ambiguous',
          },
        },
      },
      server_auctions: [],
      slot_correlations: [],
      gpt_diagnostics: {
        schema_version: 1,
        source_schema_version: 1,
        capturedAt,
        page: { origin, pathname: '/[redacted]' },
        slots: [],
        callbackIssues: [],
        coverage: {
          slotRequested: counts,
          slotResponseReceived: counts,
          slotRenderEnded: counts,
          slotOnload: counts,
          impressionViewable: counts,
          slotVisibilityChanged: counts,
        },
        metadata: {
          droppedCallbacks: 0,
          evictedSlots: 0,
          evictedRequestCycles: 0,
        },
      },
      auction_coverage: { capture_status: 'not_observed', issues: [] },
      truncation: {
        omitted_server_auctions: 0,
        omitted_slot_correlations: 0,
        omitted_request_cycles: 0,
        omitted_callback_issues: 0,
        omitted_attribution_issues: 0,
        omitted_nested_values: 0,
      },
    },
  }
}
