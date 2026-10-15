import { describe, expect, it } from 'vitest';

import type { TraceEvidenceView } from '../../src/trace/correlation';
import type { TraceReportV1 } from '../../src/trace/report-types';
import { summarizeTraceReport } from '../../src/trace/view-model';

import { AUCTION_TOKEN, SLOT_TOKEN, reportFixture } from './fixtures';

type Auction = TraceReportV1['server_auctions'][number];
type Cycle = TraceReportV1['gpt_diagnostics']['slots'][number]['requests'][number];

function auction(overrides: Partial<Auction> = {}): Auction {
  return {
    schema_version: 1,
    diagnostic_auction_id: AUCTION_TOKEN,
    source: 'initial_navigation_ssat',
    terminal_status: 'completed',
    total_time_ms: 10,
    provider_calls: [
      { provider_number: 1, role: 'bidder', status: 'no_bid', returned_bid_count: 0 },
    ],
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
    ...overrides,
  } as Auction;
}

function report(auctions: Auction[] = [], cycles?: Partial<Cycle>[]): TraceReportV1 {
  const base = reportFixture() as unknown as TraceReportV1;
  const template = base.gpt_diagnostics.slots[0];
  const slots = (cycles ?? [{}]).map((cycle, index) => ({
    ...template,
    runtimeSlotNumber: index + 1,
    requests: [{ ...template.requests[0], ...cycle } as Cycle],
  }));
  return {
    ...base,
    server_auctions: auctions,
    gpt_diagnostics: { ...base.gpt_diagnostics, slots },
  } as TraceReportV1;
}

describe('trace report summary', () => {
  it('counts filled slots and bidder-only bids without naming a winner', () => {
    const summary = summarizeTraceReport(
      report(
        [
          auction({
            provider_calls: [
              { provider_number: 1, role: 'bidder', status: 'success', returned_bid_count: 2 },
              { provider_number: 2, role: 'mediator', status: 'success', returned_bid_count: 2 },
            ],
          } as Partial<Auction>),
        ],
        [{ isEmpty: false }, { isEmpty: true }]
      ),
      undefined
    );
    expect(summary.headline).toBe('1 of 2 ad slots filled · 2 Trusted Server bids');
    expect(summary.reading).toContain('2 bids were returned to Trusted Server.');
    expect(summary.reading.join(' ')).not.toMatch(/won|winner/i);
    expect(summary.fills).toEqual([
      { fill: 'filled', count: 1 },
      { fill: 'empty', count: 1 },
    ]);
  });

  it('reports incomplete auctions, provider failures and unlinked slots', () => {
    const summary = summarizeTraceReport(
      report([
        auction({
          provider_calls: [
            { provider_number: 1, role: 'bidder', status: 'error', returned_bid_count: 0 },
            { provider_number: 2, role: 'bidder', status: 'no_bid', returned_bid_count: 0 },
          ],
        } as Partial<Auction>),
        auction({ terminal_status: 'execution_failed' } as Partial<Auction>),
      ]),
      undefined
    );
    expect(summary.reading).toEqual([
      '1 of 2 server auctions completed; 1 execution failed.',
      'No bids were returned to Trusted Server.',
      '1 of 3 provider calls failed.',
      'GPT classified the filled ads as 1 backfill.',
      '1 of 1 ad slot could not be linked to a server auction.',
    ]);
    expect(summary.needsAttention).toBe(true);
    expect(summary.stats.map((stat) => stat.value)).toEqual([2, 1, 0]);
  });

  it('names one, two and many completed auctions naturally', () => {
    const phrase = (count: number): string | undefined =>
      summarizeTraceReport(report(Array.from({ length: count }, () => auction())), undefined)
        .reading[0];
    expect([phrase(1), phrase(2), phrase(3)]).toEqual([
      'The server auction completed.',
      'Both server auctions completed.',
      'All 3 server auctions completed.',
    ]);
  });

  it('names the recorded outcome of auctions that did not complete', () => {
    const summary = summarizeTraceReport(
      report([
        auction(),
        auction({
          terminal_status: 'skipped',
          terminal_reason: 'no_eligible_slots',
        } as Partial<Auction>),
        auction({
          terminal_status: 'skipped',
          terminal_reason: 'no_eligible_slots',
        } as Partial<Auction>),
        auction({ terminal_status: 'abandoned' } as Partial<Auction>),
      ]),
      undefined
    );
    expect(summary.reading[0]).toBe(
      '1 of 4 server auctions completed; 2 skipped (no eligible slots), 1 abandoned.'
    );
  });

  it('explains a page without auctions or slots', () => {
    const empty = report();
    const summary = summarizeTraceReport(
      { ...empty, gpt_diagnostics: { ...empty.gpt_diagnostics, slots: [] } },
      undefined
    );
    expect(summary.headline).toBe('No GPT ad slots observed');
    expect(summary.reading).toEqual(['No server auction was captured for this page.']);
    expect(summary.slots).toEqual([]);
  });

  it('flags empty, unknown, incomplete and creative-failure slots', () => {
    const summary = summarizeTraceReport(
      report(
        [],
        [
          { isEmpty: true, trustedServerCreativeFailures: [] },
          { isEmpty: undefined, trustedServerCreativeFailures: [] },
          { isEmpty: false, incompleteSequence: true, trustedServerCreativeFailures: [] },
          { isEmpty: false, trustedServerCreativeFailures: ['cache_fetch_failed'] },
          { isEmpty: false, incompleteSequence: false, trustedServerCreativeFailures: [] },
        ]
      ),
      undefined
    );
    expect(summary.slots.map((slot) => slot.attention)).toEqual([
      ['GPT returned no ad'],
      ['No fill observed'],
      ['GPT event sequence incomplete'],
      ['Creative bridge failure observed'],
      [],
    ]);
  });

  it('weights timing segments proportionally, clamped to 1-20, preferring viewable', () => {
    const [viewable] = summarizeTraceReport(
      report(
        [],
        [
          {
            durations: {
              requestToResponseMs: 900,
              responseToRenderMs: 1,
              renderToLoadMs: 50,
              renderToViewableMs: 1099,
            },
          },
        ]
      ),
      undefined
    ).slots;
    expect(viewable.timing.map((segment) => [segment.kind, segment.weight])).toEqual([
      ['response', 9],
      ['render', 1],
      ['viewable', 11],
    ]);
    const [loaded] = summarizeTraceReport(
      report([], [{ durations: { requestToResponseMs: 10, renderToLoadMs: 30 } }]),
      undefined
    ).slots;
    expect(loaded.timing.map((segment) => segment.kind)).toEqual(['response', 'load']);
    const [none] = summarizeTraceReport(report([], [{ durations: {} }]), undefined).slots;
    expect(none.timing).toEqual([]);
  });

  it('links a slot only through an exact matched correlation', () => {
    const base = report([auction()]);
    const joined = {
      auctions: [
        {
          evidence: base.server_auctions[0],
          sourceLabel: 'Initial-page server auction (SSAT)',
          slots: [
            {
              serverSlot: base.server_auctions[0].slots[0],
              correlation: 'matched',
              runtimeSlotNumber: 1,
              requestNumber: 1,
            },
          ],
        },
      ],
    } as unknown as TraceEvidenceView;
    const [card] = summarizeTraceReport(base, joined).slots;
    expect(card.link).toEqual({
      auctionNumber: 1,
      sourceLabel: 'Initial-page server auction (SSAT)',
      serverSlotNumber: 1,
      candidate: 'no_candidate',
    });
    const unmatched = {
      auctions: [
        {
          ...joined.auctions[0],
          slots: [{ ...joined.auctions[0].slots[0], correlation: 'unknown' }],
        },
      ],
    } as unknown as TraceEvidenceView;
    expect(summarizeTraceReport(base, unmatched).slots[0].link).toBeUndefined();
  });
});
