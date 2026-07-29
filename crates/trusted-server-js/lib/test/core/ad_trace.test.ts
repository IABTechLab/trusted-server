import { describe, expect, it } from 'vitest';
import {
  AD_TRACE_MAX_EVENTS,
  AD_TRACE_MAX_GENERATIONS,
  AD_TRACE_MAX_RENDERS,
  AD_TRACE_MAX_SLOTS,
  createAdTraceStore,
  terminalSummaryStageOutcome,
} from '../../src/core/ad_trace';

const BID_TRACE_ID = '550e8400-e29b-41d4-a716-446655440000';

describe('ad trace reducer', () => {
  it('bounds events, slots, and retained generations', () => {
    let now = 0;
    const store = createAdTraceStore(() => ++now);
    for (let i = 0; i < AD_TRACE_MAX_EVENTS + 1; i++) {
      store.record({ kind: 'prebid_auction_init', reason: 'observed' });
    }
    for (let i = 0; i < AD_TRACE_MAX_SLOTS + 1; i++) {
      store.nextGeneration(`slot-${i}`);
    }
    for (let i = 0; i < AD_TRACE_MAX_GENERATIONS + 1; i++) {
      store.nextGeneration('latest-slot');
    }

    const exported = store.export();
    expect(exported.events).toHaveLength(AD_TRACE_MAX_EVENTS);
    expect(exported.metadata.droppedEvents).toBe(1);
    expect(exported.slots).toHaveLength(AD_TRACE_MAX_SLOTS);
    expect(store.getSlot('latest-slot')?.generations).toHaveLength(AD_TRACE_MAX_GENERATIONS);
    expect(exported.metadata.evictedSlots).toBeGreaterThan(0);
  });

  it('keeps the four stages independent and only acknowledges an exact load event', () => {
    const store = createAdTraceStore(() => 10);
    const generation = store.nextGeneration('slot-a');
    store.record({
      kind: 'ts_winner_observed',
      slotId: 'slot-a',
      generation,
      bidTraceId: BID_TRACE_ID,
    });
    store.record({
      kind: 'gpt_slot_render_ended',
      slotId: 'slot-a',
      generation,
      bidTraceId: BID_TRACE_ID,
      isEmpty: false,
    });

    expect(store.getSlot('slot-a')?.stages.gam.outcome).toBe('trusted_server_candidate');
    expect(store.getSlot('slot-a')?.stages.creative.outcome).toBe('not_observed');

    store.record({
      kind: 'creative_load_acknowledged',
      slotId: 'slot-a',
      generation,
      bidTraceId: BID_TRACE_ID,
    });
    const slot = store.getSlot('slot-a');
    expect(slot?.stages.gam).toMatchObject({
      outcome: 'trusted_server_won',
      confidence: 'definitive',
    });
    expect(slot?.stages.creative).toMatchObject({
      outcome: 'load_acknowledged',
      confidence: 'definitive',
    });
  });

  it('updates only the acknowledged retained generation, never the latest generation', () => {
    const store = createAdTraceStore(() => 1);
    const first = store.nextGeneration('slot-a');
    const second = store.nextGeneration('slot-a');
    store.record({
      kind: 'creative_load_acknowledged',
      slotId: 'slot-a',
      generation: first,
      bidTraceId: BID_TRACE_ID,
    });

    const slot = store.getSlot('slot-a');
    expect(slot?.latestGeneration).toBe(second);
    expect(slot?.stages.creative.outcome).toBe('not_observed');
    expect(slot?.generations[0].stages.creative.outcome).toBe('load_acknowledged');
    expect(slot?.generations[1].stages.creative.outcome).toBe('not_observed');
  });

  it('never downgrades a definitive acknowledgement with a later GPT callback', () => {
    const store = createAdTraceStore(() => 1);
    const generation = store.nextGeneration('slot-a');
    store.record({
      kind: 'creative_load_acknowledged',
      slotId: 'slot-a',
      generation,
      bidTraceId: BID_TRACE_ID,
    });
    store.record({
      kind: 'gpt_slot_render_ended',
      slotId: 'slot-a',
      generation,
      bidTraceId: BID_TRACE_ID,
      isEmpty: false,
    });

    expect(store.getSlot('slot-a')?.stages.gam.outcome).toBe('trusted_server_won');
    expect(store.getSlot('slot-a')?.stages.creative.outcome).toBe('load_acknowledged');
  });

  it('preserves acknowledged terminal history when its generation is later cleaned up', () => {
    const store = createAdTraceStore(() => 1);
    const generation = store.nextGeneration('slot-a');
    store.record({
      kind: 'creative_load_acknowledged',
      slotId: 'slot-a',
      generation,
      bidTraceId: BID_TRACE_ID,
    });
    store.record({
      kind: 'generation_superseded',
      slotId: 'slot-a',
      generation,
      reason: 'slot_destroyed',
    });

    expect(store.getSlot('slot-a')?.stages.gam.outcome).toBe('trusted_server_won');
    expect(store.getSlot('slot-a')?.stages.creative.outcome).toBe('load_acknowledged');
  });

  it('does not rewrite a retained generation when the next auction seeds server evidence', () => {
    const store = createAdTraceStore(() => 1);
    store.record({
      kind: 'ts_winner_observed',
      slotId: 'slot-a',
      bidTraceId: BID_TRACE_ID,
    });
    const first = store.nextGeneration('slot-a');
    store.record({
      kind: 'ts_auction_observed',
      slotId: 'slot-a',
      outcome: 'no_bid',
      confidence: 'definitive',
      reason: 'terminal_summary',
    });
    const second = store.nextGeneration('slot-a');

    const slot = store.getSlot('slot-a');
    expect(
      slot?.generations.find((item) => item.generation === first)?.stages.trustedServer.outcome
    ).toBe('won');
    expect(
      slot?.generations.find((item) => item.generation === second)?.stages.trustedServer.outcome
    ).toBe('no_bid');
  });

  it('retains a Trusted Server Prebid selection when bidWon arrives without claiming creative load', () => {
    const store = createAdTraceStore(() => 1);
    const generation = store.nextGeneration('slot-a');
    store.record({
      kind: 'prebid_targeting_selected',
      slotId: 'slot-a',
      generation,
      bidTraceId: BID_TRACE_ID,
      outcome: 'won',
      confidence: 'definitive',
      reason: 'selected_targeting',
    });
    store.record({
      kind: 'prebid_bid_won',
      slotId: 'slot-a',
      generation,
      bidTraceId: BID_TRACE_ID,
    });

    expect(store.getSlot('slot-a')?.stages.prebid).toMatchObject({
      outcome: 'won',
      reason: 'selected_targeting_with_bid_won',
    });
    expect(store.getSlot('slot-a')?.stages.creative.outcome).toBe('not_observed');
  });

  it('preserves the direct iframe acknowledgement boundary without claiming GAM selection', () => {
    const store = createAdTraceStore(() => 1);
    const generation = store.nextGeneration('slot-a');
    store.record({
      kind: 'creative_load_acknowledged',
      slotId: 'slot-a',
      generation,
      bidTraceId: BID_TRACE_ID,
      reason: 'direct_iframe_load',
    });

    expect(store.getSlot('slot-a')?.stages.creative).toMatchObject({
      outcome: 'load_acknowledged',
      reason: 'direct_iframe_load',
    });
    expect(store.getSlot('slot-a')?.stages.gam.outcome).toBe('not_observed');
  });

  it('classifies overlap, client Prebid, APS, no-bid, and superseded states', () => {
    const store = createAdTraceStore(() => 1);
    const generation = store.nextGeneration('slot-a');
    store.record({
      kind: 'prebid_targeting_selected',
      slotId: 'slot-a',
      generation,
      outcome: 'client_bid_won',
      confidence: 'definitive',
      reason: 'selected_targeting',
    });
    store.record({
      kind: 'prebid_bid_won',
      slotId: 'slot-a',
      generation,
    });
    store.record({
      kind: 'gpt_slot_render_ended',
      slotId: 'slot-a',
      generation,
      isEmpty: false,
    });
    expect(store.getSlot('slot-a')?.stages.gam.outcome).toBe('client_prebid_candidate');

    store.record({ kind: 'aps_display_bids_set', slotId: 'slot-a', generation });
    expect(store.getSlot('slot-a')?.stages.gam.outcome).toBe('client_prebid_candidate');

    store.record({
      kind: 'generation_superseded',
      slotId: 'slot-a',
      generation,
      reason: 'slot_destroyed',
    });
    expect(store.getSlot('slot-a')?.stages.creative.outcome).toBe('not_observed');
    expect(store.getSlot('slot-a')?.stages.gam.outcome).toBe('client_prebid_candidate');
  });

  it('does not downgrade definitive stage evidence during service or cleanup', () => {
    const store = createAdTraceStore(() => 1);
    const generation = store.nextGeneration('slot-a');
    store.record({ kind: 'prebid_render_failed', slotId: 'slot-a', generation });
    store.record({ kind: 'pb_render_served', slotId: 'slot-a', generation });
    store.record({
      kind: 'generation_superseded',
      slotId: 'slot-a',
      generation,
      reason: 'navigation',
    });

    expect(store.getSlot('slot-a')?.stages.creative).toMatchObject({
      outcome: 'render_failed',
      confidence: 'definitive',
    });
  });

  it('does not downgrade a definitive empty render outcome', () => {
    const store = createAdTraceStore(() => 1);
    const generation = store.nextGeneration('slot-a');
    store.record({
      kind: 'gpt_slot_render_ended',
      slotId: 'slot-a',
      generation,
      isEmpty: true,
    });
    store.record({
      kind: 'generation_superseded',
      slotId: 'slot-a',
      generation,
      reason: 'navigation',
    });
    store.record({ kind: 'pb_render_served', slotId: 'slot-a', generation });

    expect(store.getRenderTimeline()[0]).toMatchObject({
      outcome: 'empty',
      confidence: 'definitive',
    });
  });

  it('enriches one bounded render record and keeps visibility independent', () => {
    let now = 0;
    const store = createAdTraceStore(() => ++now);
    const generation = store.nextGeneration('slot-a');
    store.record({
      kind: 'gpt_slot_render_ended',
      slotId: 'slot-a',
      generation,
      isEmpty: false,
    });
    store.record({
      kind: 'pb_render_served',
      slotId: 'slot-a',
      generation,
      reason: 'pb_render_response',
    });
    store.record({
      kind: 'creative_load_acknowledged',
      slotId: 'slot-a',
      generation,
      bidTraceId: BID_TRACE_ID,
    });
    store.updateVisibility('slot-a', generation, 'hidden');

    const timeline = store.getRenderTimeline();
    expect(timeline).toHaveLength(1);
    expect(timeline[0]).toMatchObject({
      sequence: 1,
      outcome: 'confirmed',
      confidence: 'definitive',
      visibility: 'hidden',
    });
    store.updateVisibility('slot-a', generation, 'visible');
    expect(store.getRenderTimeline()[0]).toMatchObject({
      sequence: 1,
      outcome: 'confirmed',
      confidence: 'definitive',
      visibility: 'visible',
    });
  });

  it('keeps GPT viewability separate from element visibility and creative load', () => {
    const store = createAdTraceStore(() => 1);
    const generation = store.nextGeneration('slot-a');
    store.record({
      kind: 'gpt_slot_render_ended',
      slotId: 'slot-a',
      generation,
      isEmpty: false,
    });
    store.updateVisibility('slot-a', generation, 'hidden');
    store.record({ kind: 'gpt_impression_viewable', slotId: 'slot-a', generation });

    expect(store.getRenderTimeline()[0]).toMatchObject({
      outcome: 'gam_only',
      visibility: 'hidden',
      viewability: 'viewable',
    });
    expect(store.getSlot('slot-a')?.stages.creative.outcome).toBe('not_observed');
  });

  it('distinguishes APS renderer start from the validated ready boundary', () => {
    const store = createAdTraceStore(() => 1);
    const generation = store.nextGeneration('slot-a');
    store.record({
      kind: 'pb_render_served',
      slotId: 'slot-a',
      generation,
      reason: 'direct_aps_renderer',
    });
    expect(store.getSlot('slot-a')?.stages.creative).toMatchObject({
      outcome: 'renderer_served',
      reason: 'direct_aps_renderer',
    });
    expect(store.getRenderTimeline()[0]).toMatchObject({
      outcome: 'served',
      reason: 'direct_aps_renderer',
    });

    store.record({
      kind: 'aps_renderer_ready',
      slotId: 'slot-a',
      generation,
      reason: 'direct_aps_renderer_ready',
    });
    expect(store.getSlot('slot-a')?.stages.creative).toMatchObject({
      outcome: 'aps_renderer_ready',
      reason: 'direct_aps_renderer_ready',
    });
    expect(store.getRenderTimeline()[0]).toMatchObject({
      outcome: 'served',
      reason: 'direct_aps_renderer_ready',
    });
  });

  it('dispatches a frozen privacy-safe render event', () => {
    const store = createAdTraceStore(() => 1);
    const observed: unknown[] = [];
    const listener = (event: Event) => observed.push((event as CustomEvent).detail);
    window.addEventListener('tsjs:adRendered', listener);
    const generation = store.nextGeneration('slot-a');
    store.record({
      kind: 'pb_render_served',
      slotId: 'slot-a',
      generation,
      reason: 'pb_render_response',
      rawUrl: 'https://private.example',
    } as never);
    window.removeEventListener('tsjs:adRendered', listener);

    expect(observed).toHaveLength(1);
    expect(Object.isFrozen(observed[0])).toBe(true);
    expect(JSON.stringify(observed[0])).not.toContain('private.example');
  });

  it('bounds the render timeline without duplicating impression generations', () => {
    const store = createAdTraceStore(() => 1);
    for (let i = 0; i < AD_TRACE_MAX_RENDERS + 1; i++) {
      const slotId = `render-${i}`;
      const generation = store.nextGeneration(slotId);
      store.record({ kind: 'gpt_request_started', slotId, generation });
    }
    expect(store.getRenderTimeline()).toHaveLength(AD_TRACE_MAX_RENDERS);
    expect(store.getRenderTimeline()[0].slotId).toBe('render-1');
  });

  it('preserves failed and abandoned terminal summaries while mapping completed no-winner to no bid', () => {
    expect(terminalSummaryStageOutcome('completed')).toBe('no_bid');
    expect(terminalSummaryStageOutcome('completed', true)).toBe('completed');
    expect(terminalSummaryStageOutcome('failed')).toBe('failed');
    expect(terminalSummaryStageOutcome('abandoned')).toBe('abandoned');
    expect(terminalSummaryStageOutcome('skipped')).toBe('skipped');
  });

  it('rejects malformed runtime event kinds and confidence values', () => {
    const store = createAdTraceStore(() => 1);
    store.record({ kind: 'not-a-real-kind', slotId: 'slot-a' } as never);
    store.record({
      kind: 'ts_winner_observed',
      slotId: 'slot-a',
      confidence: 'certain',
    } as never);
    expect(store.getEvents()).toHaveLength(0);
  });

  it('exports an immutable sanitized clone', () => {
    const store = createAdTraceStore(() => 1);
    store.record({
      kind: 'pb_render_rejected',
      slotId: 'slot-a',
      reason: 'missing_generation',
      // Ensure unknown private fields cannot enter the public export.
      rawUrl: 'https://private.example/path',
    } as never);

    const exported = store.export();
    expect(Object.isFrozen(exported)).toBe(true);
    expect(JSON.stringify(exported)).not.toContain('private.example');
    expect(() => exported.events.push({} as never)).toThrow();
  });
});
