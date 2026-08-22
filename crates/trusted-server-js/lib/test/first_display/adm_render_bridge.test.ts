import { describe, expect, it, vi } from 'vitest';
import { JSDOM } from 'jsdom';

import type { FirstDisplayGptBoundCycleV1 } from '../../src/first_display/adapters/googletag';

import { createTestFirstDisplayRenderBridge } from './helpers/render_owner_composition';

const RESERVATION_ID = `r1_${'a'.repeat(22)}`;

function harness() {
  const dom = new JSDOM('<!doctype html><div id="slot-1"></div>', {
    url: 'https://publisher.example/',
  });
  const element = dom.window.document.getElementById('slot-1');
  if (!(element instanceof dom.window.HTMLElement)) throw new Error('missing fixture element');
  const cycle: FirstDisplayGptBoundCycleV1 = Object.freeze({
    bid: Object.freeze({
      candidateId: 'candidate001',
      slot: 'slot-1',
      provider: 'example',
      upstreamBidId: 'upstream-1',
      cpm: 1.25,
      currency: 'USD' as const,
      targeting: Object.freeze({}),
      rendererReservationId: RESERVATION_ID,
      renderSource: Object.freeze({
        type: 'adm' as const,
        version: 1 as const,
        adm: '<main>fictional creative</main>',
        width: 300,
        height: 250,
      }),
    }),
    element,
    isCurrent: () => true,
    ownership: 'trusted_server',
    physicalSlot: {},
    placement: Object.freeze({
      slot: 'slot-1',
      gamUnitPath: '/123/example',
      divId: 'slot-1',
      formats: Object.freeze([Object.freeze([300, 250] as const)]),
      targeting: Object.freeze({}),
    }),
    slotId: 'slot-1',
    traceToken: 'gt1_1',
  });
  const timers = new Map<object, () => void>();
  let now = 0;
  const terminal = vi.fn();
  const mutation = vi.fn(() => true);
  const bridge = createTestFirstDisplayRenderBridge({
    browser: dom.window as unknown as Window,
    clearTimer: (handle) => timers.delete(handle as object),
    createChannel: () => {
      throw new Error('ADM fallback must not create an owner channel');
    },
    document: dom.window.document,
    fillRandom: () => undefined,
    getAps: () => undefined,
    now: () => now,
    onNativeMutation: mutation,
    setTimer: (callback) => {
      const handle = {};
      timers.set(handle, callback);
      return handle;
    },
  });
  expect(bridge.bind(cycle, terminal)).toBe(true);
  return {
    bridge,
    cycle,
    dom,
    element,
    mutation,
    setNow: (value: number) => {
      now = value;
    },
    terminal,
    timers,
  };
}

describe('ADM-only shared first-display render journal', () => {
  it('owns only the attributable empty-GAM ADM frame and transfers it once', () => {
    const h = harness();

    expect(h.bridge.recordGam(h.cycle, 'gam_empty')).toBe(true);
    const frame = h.element.querySelector('iframe');
    expect(frame?.srcdoc).toContain('fictional creative');
    frame?.dispatchEvent(new h.dom.window.Event('load'));
    expect(h.terminal).toHaveBeenCalledWith('accepted', null);

    h.bridge.sealTsAdmission();
    expect(h.bridge.closeIngress()).toBe(true);
    expect(h.bridge.captureHandoff()?.artifacts).toEqual([
      {
        hostPosition: null,
        hostPositionPriority: null,
        identity: frame,
        kind: 'gpt_adm',
        owner: 'trusted_server',
        slotId: 'slot-1',
        token: RESERVATION_ID,
      },
    ]);
    expect(h.bridge.detachCommittedArtifacts()).toBe(true);
    h.bridge.dispose();
    expect(frame?.isConnected).toBe(true);
  });

  it('retires one accepted ADM frame without repeating terminal settlement', () => {
    const h = harness();
    expect(h.bridge.recordGam(h.cycle, 'gam_empty')).toBe(true);
    const frame = h.element.querySelector('iframe');
    frame?.dispatchEvent(new h.dom.window.Event('load'));
    expect(h.terminal).toHaveBeenCalledOnce();

    expect(h.bridge.retire(h.cycle)).toBe(true);
    expect(h.bridge.retire(h.cycle)).toBe(false);
    expect(frame?.isConnected).toBe(false);
    expect(h.terminal).toHaveBeenCalledOnce();
  });

  it('fails the owned frame at the bounded load deadline and rejects APS input', () => {
    const h = harness();
    expect(h.bridge.recordGam(h.cycle, 'gam_empty')).toBe(true);
    expect(h.timers).toHaveLength(1);
    [...h.timers.values()][0]?.();
    expect(h.terminal).toHaveBeenCalledWith('failed', 'adm_document_no_load');
    expect(h.element.querySelector('iframe')).toBeNull();

    const aps = Object.freeze({
      ...h.cycle,
      bid: Object.freeze({
        ...h.cycle.bid,
        rendererReservationId: `r1_${'b'.repeat(22)}`,
        renderSource: Object.freeze({
          type: 'aps' as const,
          version: 1 as const,
          accountId: 'publisher-account',
          bidId: 'bid-1',
          tagType: 'iframe' as const,
          creativeUrl: 'https://creative.example/render',
          width: 300,
          height: 250,
          aaxResponse: 'renderer-envelope',
        }),
      }),
    });
    expect(h.bridge.bind(aps, vi.fn())).toBe(false);
  });
});
