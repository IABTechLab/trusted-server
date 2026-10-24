import type { GptDiagnosticsStore } from '../../src/integrations/gpt_diagnostics/store';
import { GptDiagnosticsApiController } from '../../src/integrations/gpt_diagnostics/api';
import type { TraceCollector } from '../../src/trace/collector';
import { joinTraceEvidence } from '../../src/trace/correlation';
import { buildTraceReport } from '../../src/trace/report';

import { AUCTION_TOKEN, SLOT_TOKEN, TRACE_NOW, TRACE_ORIGIN, reportFixture } from './fixtures';

/** Returns the actual public API snapshot, retaining its optional undefined members. */
export function observedGptSource(store: GptDiagnosticsStore) {
  const controller = new GptDiagnosticsApiController(
    store,
    {
      exportBinding: () => ({ status: 'bound' }),
      subscribe: () => () => {},
    },
    { show: () => {}, hide: () => {} },
    { now: () => new Date(TRACE_NOW) }
  );
  const source = controller.snapshot();
  controller.destroy();
  return { ...source, page: { ...source.page, origin: TRACE_ORIGIN } };
}

/** Exercises the real public-cycle projection and exact V4 join with store observations. */
export function joinedGptStore(store: GptDiagnosticsStore, collector: TraceCollector) {
  const source = observedGptSource(store);
  const captured = buildTraceReport({
    requestContext: reportFixture().request_context,
    gptSource: source,
    origin: TRACE_ORIGIN,
    capturedAtMs: TRACE_NOW,
    collector: collector.snapshot().value,
  });
  if (!captured.ok) throw new Error(`should capture observed GPT cycles: ${captured.reason}`);
  return joinTraceEvidence(captured.value.report, TRACE_ORIGIN, TRACE_NOW);
}

export function gptTransport(source = 'initial_navigation_ssat') {
  return {
    schema_version: 1,
    evidence: {
      schema_version: 1,
      diagnostic_auction_id: AUCTION_TOKEN,
      source,
      terminal_status: 'completed',
      provider_calls: [],
      slots: [
        {
          slot_number: 1,
          slot_ref: SLOT_TOKEN,
          requested_sizes: [[300, 250]],
          returned_bid_count: 0,
          candidate: 'no_candidate',
        },
      ],
      truncation: { omitted_provider_calls: 0, omitted_slots: 0, omitted_nested_values: 0 },
      coverage: { provider_to_slot_no_bid: 'unavailable' },
    },
  };
}
