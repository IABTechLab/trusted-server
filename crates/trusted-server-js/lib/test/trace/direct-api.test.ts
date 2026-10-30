import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { buildAdRequest, sendAuction } from '../../src/core/auction';
import type { TsjsApi } from '../../src/core/types';
import { installTraceRuntime } from '../../src/trace/runtime';

import { SLOT_TOKEN, AUCTION_TOKEN } from './fixtures';

function request() {
  return buildAdRequest([
    { code: 'example-slot', mediaTypes: { banner: { sizes: [[300, 250]] } } },
  ]);
}
function bids() {
  return {
    seatbid: [
      {
        seat: 'example-bidder',
        bid: [
          {
            impid: 'example-slot',
            adm: '<div>Example creative</div>',
            price: 1,
            w: 300,
            h: 250,
          },
        ],
      },
    ],
  };
}
function transport() {
  return {
    schema_version: 1,
    evidence: {
      schema_version: 1,
      diagnostic_auction_id: AUCTION_TOKEN,
      source: 'auction_api',
      terminal_status: 'completed',
      provider_calls: [],
      slots: [
        {
          slot_number: 1,
          slot_ref: SLOT_TOKEN,
          requested_sizes: [[300, 250]],
          returned_bid_count: 1,
          candidate: 'selected',
          selected_creative_size: [300, 250],
        },
      ],
      truncation: {
        omitted_provider_calls: 0,
        omitted_slots: 0,
        omitted_nested_values: 0,
      },
      coverage: { provider_to_slot_no_bid: 'unavailable' },
    },
  };
}
describe('direct API trace observer preserves ordinary bids', () => {
  const fetch = vi.fn<typeof globalThis.fetch>();
  beforeEach(() => {
    window.tsjs = {} as TsjsApi;
    window.__tsjs_trace_active = true;
    vi.stubGlobal('fetch', fetch);
    fetch.mockReset();
    vi.spyOn(window.crypto, 'randomUUID').mockReturnValue('12345678-1234-4abc-8def-123456789abc');
    installTraceRuntime(window.tsjs);
  });
  afterEach(() => {
    vi.restoreAllMocks();
    vi.unstubAllGlobals();
    delete window.tsjs;
    delete window.__tsjs_trace_active;
  });
  it('decorates final grouped units and records API evidence before parsing ordinary bids', async () => {
    let parsedBids = false;
    const result = {
      ...bids(),
      ext: { trusted_server: { trace_auction: transport() } },
    };
    Object.defineProperty(result, 'seatbid', {
      get() {
        parsedBids = true;
        expect(window.tsjs?.traceEvidence?.snapshot().value?.serverAuctions).toHaveLength(1);
        return bids().seatbid;
      },
    });
    fetch.mockResolvedValueOnce({
      ok: true,
      headers: new Headers({ 'content-type': 'application/json' }),
      json: async () => result,
    } as Response);
    const ordinary = request();
    const received = await sendAuction('/auction', ordinary);
    expect(parsedBids).toBe(true);
    expect(received[0]).toMatchObject({
      impid: 'example-slot',
      adm: '<div>Example creative</div>',
      width: 300,
      height: 250,
    });
    const body = JSON.parse(fetch.mock.calls[0][1]?.body as string);
    expect(body.adUnits[0].ext.trusted_server.trace_slot_ref).toBe(SLOT_TOKEN);
    expect(ordinary.adUnits[0]).not.toHaveProperty('ext');
    expect(window.tsjs?.traceEvidence?.captureStatus()).toBe('complete');
    expect(window.tsjs?.traceEvidence?.snapshot().value?.issues).toEqual([
      'correlation_unavailable',
    ]);
    expect(window.tsjs?.traceEvidence?.snapshot().value?.slotCorrelations).toEqual([]);
  });
  it('records a supplied unreadable namespace as validation failure while preserving bids', async () => {
    const trusted = new Proxy(
      { trace_auction: transport() },
      {
        getOwnPropertyDescriptor() {
          throw new Error('private-descriptor-error');
        },
      }
    );
    const response = { ...bids(), ext: { trusted_server: trusted } };
    fetch.mockResolvedValueOnce({
      ok: true,
      headers: new Headers({ 'content-type': 'application/json' }),
      json: async () => response,
    } as Response);
    expect(await sendAuction('/auction', request())).toHaveLength(1);
    expect(window.tsjs?.traceEvidence?.snapshot().value?.issues).toEqual([
      'evidence_validation_failed',
    ]);
    expect(JSON.stringify(window.tsjs?.traceEvidence?.snapshot())).not.toContain('private');
  });
  it.each([null, { schema_version: 1, unavailable_reason: 'evidence_projection_failed' }])(
    'preserves ordinary bid results when supplied evidence is invalid or unavailable',
    async (value) => {
      fetch.mockResolvedValueOnce(
        new Response(
          JSON.stringify({
            ...bids(),
            ext: { trusted_server: { trace_auction: value } },
          }),
          { headers: { 'content-type': 'application/json' } }
        )
      );
      const received = await sendAuction('/auction', request());
      expect(received).toHaveLength(1);
      expect(window.tsjs?.traceEvidence?.captureStatus()).toBe('unavailable');
      expect(window.tsjs?.traceEvidence?.snapshot().value?.issues).toEqual([
        value === null ? 'evidence_validation_failed' : 'evidence_projection_failed',
      ]);
    }
  );
  it('adds no issue for absent optional response evidence and does not reset earlier records', async () => {
    window.tsjs?.traceEvidence?.recordTransport(transport());
    fetch.mockResolvedValueOnce(
      new Response(JSON.stringify(bids()), {
        headers: { 'content-type': 'application/json' },
      })
    );
    expect(await sendAuction('/auction', request())).toHaveLength(1);
    expect(window.tsjs?.traceEvidence?.captureStatus()).toBe('complete');
  });
  it.each(['non-ok', 'unreadable-json', 'fetch-rejected', 'non-json'])(
    'records only a bounded transport failure for %s',
    async (failure) => {
      if (failure === 'non-ok')
        fetch.mockResolvedValueOnce(new Response('private-body', { status: 500 }));
      if (failure === 'unreadable-json')
        fetch.mockResolvedValueOnce(
          new Response('private-invalid-json', {
            headers: { 'content-type': 'application/json' },
          })
        );
      if (failure === 'fetch-rejected')
        fetch.mockRejectedValueOnce(new Error('private-fetch-error'));
      if (failure === 'non-json')
        fetch.mockResolvedValueOnce(
          new Response('private-body', {
            headers: { 'content-type': 'text/plain' },
          })
        );
      expect(await sendAuction('/auction', request())).toEqual([]);
      expect(window.tsjs?.traceEvidence?.snapshot().value?.issues).toEqual([
        'evidence_transport_failed',
      ]);
      expect(JSON.stringify(window.tsjs?.traceEvidence?.snapshot())).not.toContain('private');
    }
  );
  it('keeps the ordinary request and bids intact when token generation or collector callbacks throw', async () => {
    vi.mocked(window.crypto.randomUUID).mockImplementation(() => {
      throw new Error('private-random-error');
    });
    const ordinary = request();
    const prior = window.tsjs!.traceEvidence!;
    window.tsjs!.traceEvidence = {
      ...prior,
      recordTransport() {
        throw new Error('private-callback-error');
      },
    };
    fetch.mockResolvedValueOnce(
      new Response(
        JSON.stringify({
          ...bids(),
          ext: { trusted_server: { trace_auction: transport() } },
        }),
        { headers: { 'content-type': 'application/json' } }
      )
    );
    expect(await sendAuction('/auction', ordinary)).toHaveLength(1);
    expect(JSON.parse(fetch.mock.calls[0][1]?.body as string)).toEqual(ordinary);
  });
  it.each([undefined, false, 'true', 1])(
    'adds no tokens or observers for nonliteral gate %s',
    async (active) => {
      window.__tsjs_trace_active = active;
      fetch.mockResolvedValueOnce(
        new Response(
          JSON.stringify({
            ...bids(),
            ext: { trusted_server: { trace_auction: transport() } },
          }),
          { headers: { 'content-type': 'application/json' } }
        )
      );
      const ordinary = request();
      expect(await sendAuction('/auction', ordinary)).toHaveLength(1);
      expect(window.crypto.randomUUID).not.toHaveBeenCalled();
      expect(JSON.parse(fetch.mock.calls[0][1]?.body as string)).toEqual(ordinary);
      expect(window.tsjs?.traceEvidence?.captureStatus()).toBe('not_observed');
    }
  );
});
