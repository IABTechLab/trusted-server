import { describe, expect, it } from 'vitest';

import { joinTraceEvidence } from '../../src/trace/correlation';

import { reportFixture, TRACE_NOW, TRACE_ORIGIN, AUCTION_TOKEN, SLOT_TOKEN } from './fixtures';

function fixture(source = 'initial_navigation_ssat') {
  const report = reportFixture();
  return {
    ...report,
    server_auctions: [
      {
        schema_version: 1,
        diagnostic_auction_id: AUCTION_TOKEN,
        source,
        terminal_status: 'completed',
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
      },
    ],
    slot_correlations:
      source === 'auction_api'
        ? []
        : [
            {
              schema_version: 1,
              diagnostic_auction_id: AUCTION_TOKEN,
              slot_ref: SLOT_TOKEN,
              runtime_slot_number: 1,
              request_number: 1,
            },
          ],
    auction_coverage: {
      capture_status: 'complete',
      issues: source === 'auction_api' ? ['correlation_unavailable'] : [],
    },
  };
}
function view(report: ReturnType<typeof fixture>) {
  const result = joinTraceEvidence(report, TRACE_ORIGIN, TRACE_NOW);
  if (typeof result !== 'object' || result === null || !('auctions' in result))
    throw new Error('should join report');
  return result;
}
describe('exact trace evidence associations', () => {
  it.each([
    ['initial_navigation_ssat', 'Initial-page server auction (SSAT)'],
    ['spa_page_bids', 'Trusted Server page-refresh auction'],
  ])(
    'joins a supported %s record only with both tokens and exact GPT identity',
    (source, label) => {
      const report = fixture(source);
      const result = view(report);
      expect(result.auctions[0].sourceLabel).toBe(label);
      expect(result.auctions[0].slots[0].correlation).toBe('matched');
      expect(result.auctions[0].slots[0].cycle).toEqual(
        report.gpt_diagnostics.slots[0].requests[0]
      );
      expect(result.auctions[0].relativeMilestonesLabel).toBe('Unavailable in v1');
      expect(result.auctions[0].providerScopeLabel).toBe(
        'Auction-wide provider status; per-slot no-bid reason unavailable'
      );
      expect(report).toEqual(fixture(source));
    }
  );
  it('keeps API evidence independent of GPT and rejects API sidecars', () => {
    const report = fixture('auction_api');
    expect(view(report).auctions[0]).toMatchObject({
      sourceLabel: 'Trusted Server auction API',
      slots: [{ correlation: 'unknown' }],
    });
    Object.assign(report, { slot_correlations: fixture().slot_correlations });
    expect(joinTraceEvidence(report, TRACE_ORIGIN, TRACE_NOW)).toBeUndefined();
  });
  it.each([
    'missing-sidecar',
    'missing-cycle',
    'wrong-auction',
    'duplicate-sidecar',
    'duplicate-record',
    'duplicate-slot',
    'conflicting-sidecar',
    'duplicate-cycle',
  ])('leaves %s as Correlation unknown', (kind) => {
    const report = fixture();
    const raw = report as unknown as {
      server_auctions: { slots: unknown[] }[];
      slot_correlations: { diagnostic_auction_id: string; slot_ref: string }[];
    };
    if (kind === 'missing-sidecar') raw.slot_correlations = [];
    if (kind === 'missing-cycle') report.gpt_diagnostics.slots[0].requests = [];
    if (kind === 'wrong-auction')
      report.gpt_diagnostics.slots[0].requests[0].trustedServerAuctionId =
        'ts-auc-0000000112344abc8def123456789abc';
    if (kind === 'duplicate-sidecar')
      raw.slot_correlations.push(structuredClone(raw.slot_correlations[0]));
    if (kind === 'duplicate-record')
      raw.server_auctions.push(structuredClone(raw.server_auctions[0]));
    if (kind === 'duplicate-slot')
      raw.server_auctions[0].slots.push(structuredClone(raw.server_auctions[0].slots[0]));
    if (kind === 'conflicting-sidecar')
      raw.slot_correlations.push({
        ...raw.slot_correlations[0],
        slot_ref: 'ts-slot-00000001-1234-4abc-8def-123456789abc',
      });
    if (kind === 'duplicate-cycle')
      report.gpt_diagnostics.slots[0].requests.push(
        structuredClone(report.gpt_diagnostics.slots[0].requests[0])
      );
    expect(view(report).auctions[0].slots[0].correlation).toBe('unknown');
  });
  it.each([
    ['prebid_refresh', 'Browser refresh observed; winner not determined'],
    ['publisher_refresh', 'Browser refresh observed; winner not determined'],
    ['competing', 'Multiple or unknown delivery paths'],
    ['unattributed', 'Multiple or unknown delivery paths'],
  ])('preserves %s path meaning rather than inferring a winner', (path, label) => {
    const report = fixture();
    Object.assign(report.gpt_diagnostics.slots[0].requests[0], {
      requestPath: path,
      trustedServerCreativeResponseAtMs: undefined,
    });
    Reflect.deleteProperty(
      report.gpt_diagnostics.slots[0].requests[0],
      'trustedServerCreativeResponseAtMs'
    );
    const slot = view(report).auctions[0].slots[0];
    expect(slot.pathLabel).toBe(label);
    expect(slot.creativeLabel).not.toBe('Trusted Server creative rendered');
  });
  it('requires matched nonempty render and creative-bridge evidence for the participation label', () => {
    const report = fixture();
    expect(view(report).auctions[0].slots[0].creativeLabel).toBe(
      'Trusted Server creative rendered'
    );
    Object.assign(report.gpt_diagnostics.slots[0].requests[0], { isEmpty: true });
    expect(view(report).auctions[0].slots[0].creativeLabel).toBe('Participation unconfirmed');
  });
  it('cannot claim participation from a selected candidate and GPT fill alone', () => {
    const report = fixture();
    report.server_auctions[0].slots[0].candidate = 'selected';
    Reflect.deleteProperty(
      report.gpt_diagnostics.slots[0].requests[0],
      'trustedServerCreativeResponseAtMs'
    );
    expect(view(report).auctions[0].slots[0].creativeLabel).toBe('Participation unconfirmed');
  });
});
