import type { TraceGptRequestCycle } from './report-types';
import { parseTraceReport } from './report-validation';
import type { TraceAuctionEvidenceV1, TraceAuctionSlot } from './types';

/** Server candidate facts remain independent when the exact browser join is unknown. */
export interface TraceSlotEvidenceView {
  readonly serverSlot: TraceAuctionSlot;
  readonly correlation: 'matched' | 'unknown';
  readonly runtimeSlotNumber?: number;
  readonly requestNumber?: number;
  readonly cycle?: TraceGptRequestCycle;
  readonly pathLabel?: string;
  readonly creativeLabel?: string;
}
export interface TraceAuctionEvidenceView {
  readonly evidence: TraceAuctionEvidenceV1;
  readonly sourceLabel: string;
  readonly relativeMilestonesLabel: 'Unavailable in v1';
  readonly providerScopeLabel: 'Auction-wide provider status; per-slot no-bid reason unavailable';
  readonly slots: readonly TraceSlotEvidenceView[];
}
export interface TraceEvidenceView {
  readonly auctions: readonly TraceAuctionEvidenceView[];
}
function pathLabel(cycle: TraceGptRequestCycle): string {
  switch (cycle.requestPath) {
    case 'prebid_refresh':
    case 'publisher_refresh':
      return 'Browser refresh observed; winner not determined';
    case 'competing':
    case 'unattributed':
      return 'Multiple or unknown delivery paths';
    case 'trusted_server_direct':
      return 'Trusted Server request path observed';
    default:
      return 'Request path unavailable';
  }
}
const SOURCES = {
  initial_navigation_ssat: 'Initial-page server auction (SSAT)',
  spa_page_bids: 'Trusted Server page-refresh auction',
  auction_api: 'Trusted Server auction API',
} as const;

/** Joins only unique token pairs and exact exported GPT request-cycle identities. */
export function joinTraceEvidence(
  value: unknown,
  origin: string,
  capturedAtMs: number
): TraceEvidenceView | undefined {
  const report = parseTraceReport(value, origin, capturedAtMs);
  if (!report) return undefined;
  const cycles = report.gpt_diagnostics.slots.flatMap((slot) =>
    slot.requests.map((cycle) => ({ runtimeSlotNumber: slot.runtimeSlotNumber, cycle }))
  );
  const serverSlots = report.server_auctions.flatMap((auction) =>
    auction.slots.map((slot) => ({ id: auction.diagnostic_auction_id, slot }))
  );
  const auctions = report.server_auctions.map((auction) => {
    const id = auction.diagnostic_auction_id;
    const uniqueAuction =
      report.server_auctions.filter((record) => record.diagnostic_auction_id === id).length === 1;
    const slots = auction.slots.map((slot): TraceSlotEvidenceView => {
      const unknown: TraceSlotEvidenceView = Object.freeze({
        serverSlot: slot,
        correlation: 'unknown',
      });
      if (!uniqueAuction || auction.source === 'auction_api') return unknown;
      if (
        serverSlots.filter((record) => record.id === id && record.slot.slot_ref === slot.slot_ref)
          .length !== 1
      )
        return unknown;
      const sidecars = report.slot_correlations.filter(
        (sidecar) => sidecar.diagnostic_auction_id === id && sidecar.slot_ref === slot.slot_ref
      );
      if (sidecars.length !== 1) return unknown;
      const sidecar = sidecars[0];
      if (
        report.slot_correlations.filter(
          (record) =>
            record.runtime_slot_number === sidecar.runtime_slot_number &&
            record.request_number === sidecar.request_number
        ).length !== 1
      )
        return unknown;
      const matches = cycles.filter(
        (record) =>
          record.runtimeSlotNumber === sidecar.runtime_slot_number &&
          record.cycle.requestNumber === sidecar.request_number
      );
      if (matches.length !== 1 || matches[0].cycle.trustedServerAuctionId !== id) return unknown;
      const cycle = matches[0].cycle;
      const participated =
        cycle.isEmpty === false &&
        cycle.renderAtMs !== undefined &&
        cycle.trustedServerCreativeResponseAtMs !== undefined &&
        cycle.delivery === 'trusted_server_response_sent';
      return Object.freeze({
        serverSlot: slot,
        correlation: 'matched',
        runtimeSlotNumber: sidecar.runtime_slot_number,
        requestNumber: sidecar.request_number,
        cycle,
        pathLabel: pathLabel(cycle),
        creativeLabel: participated
          ? 'Trusted Server creative rendered'
          : 'Participation unconfirmed',
      });
    });
    return Object.freeze({
      evidence: auction,
      sourceLabel: SOURCES[auction.source],
      relativeMilestonesLabel: 'Unavailable in v1' as const,
      providerScopeLabel:
        'Auction-wide provider status; per-slot no-bid reason unavailable' as const,
      slots: Object.freeze(slots),
    });
  });
  return Object.freeze({ auctions: Object.freeze(auctions) });
}
