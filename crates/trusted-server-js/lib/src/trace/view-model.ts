import type { TraceEvidenceView } from './correlation';
import type { TraceGptRequestCycle, TraceReportV1 } from './report-types';

/** Fill state of one GPT slot across all of its retained request cycles. */
export type TraceSlotFill = 'filled' | 'empty' | 'unknown';

/** One proportional segment of a slot's request timing bar. */
export interface TraceTimingSegment {
  readonly kind: 'response' | 'render' | 'load' | 'viewable';
  readonly label: string;
  readonly ms: number;
  /** Relative flex weight from 1 to 20; exact milliseconds stay in `ms`. */
  readonly weight: number;
}

/** Server auction a slot was exactly joined to through a correlation record. */
export interface TraceSlotLink {
  readonly auctionNumber: number;
  readonly sourceLabel: string;
  readonly serverSlotNumber: number;
  readonly candidate: TraceReportV1['server_auctions'][number]['slots'][number]['candidate'];
}

/** Card-level facts for one GPT slot. */
export interface TraceSlotCard {
  readonly runtimeSlotNumber: number;
  readonly fill: TraceSlotFill;
  readonly backfill: boolean;
  readonly size?: readonly [number, number];
  readonly requestCount: number;
  readonly timingRequestNumber?: number;
  readonly timing: readonly TraceTimingSegment[];
  readonly link?: TraceSlotLink;
  readonly attention: readonly string[];
}

/** Plain-language counts shown before any detailed fact list. */
export interface TraceSummary {
  readonly headline: string;
  readonly reading: readonly string[];
  readonly needsAttention: boolean;
  readonly stats: readonly { readonly value: number; readonly label: string }[];
  readonly fills: readonly { readonly fill: TraceSlotFill; readonly count: number }[];
  readonly slots: readonly TraceSlotCard[];
}

const MAX_WEIGHT = 20;

function plural(count: number, one: string, many: string): string {
  return `${count} ${count === 1 ? one : many}`;
}

function fillOf(requests: readonly TraceGptRequestCycle[]): TraceSlotFill {
  if (requests.some((cycle) => cycle.isEmpty === false)) return 'filled';
  if (requests.some((cycle) => cycle.isEmpty === true)) return 'empty';
  return 'unknown';
}

/** The first filled request explains the slot best; otherwise the latest attempt. */
function representative(
  requests: readonly TraceGptRequestCycle[]
): TraceGptRequestCycle | undefined {
  return requests.find((cycle) => cycle.isEmpty === false) ?? requests[requests.length - 1];
}

function timing(cycle: TraceGptRequestCycle | undefined): TraceTimingSegment[] {
  if (!cycle) return [];
  const { durations } = cycle;
  const candidates: [TraceTimingSegment['kind'], string, number | undefined][] = [
    ['response', 'response', durations.requestToResponseMs],
    ['render', 'render', durations.responseToRenderMs],
    durations.renderToViewableMs !== undefined
      ? ['viewable', 'until viewable', durations.renderToViewableMs]
      : ['load', 'until loaded', durations.renderToLoadMs],
  ];
  const raw: [TraceTimingSegment['kind'], string, number][] = [];
  for (const [kind, label, ms] of candidates)
    if (typeof ms === 'number' && Number.isFinite(ms) && ms >= 0) raw.push([kind, label, ms]);
  const total = raw.reduce((sum, [, , ms]) => sum + ms, 0);
  return raw.map(([kind, label, ms]) => ({
    kind,
    label,
    ms,
    weight:
      total > 0 ? Math.min(MAX_WEIGHT, Math.max(1, Math.round((ms / total) * MAX_WEIGHT))) : 1,
  }));
}

function attentionFor(fill: TraceSlotFill, requests: readonly TraceGptRequestCycle[]): string[] {
  const reasons: string[] = [];
  if (fill === 'empty') reasons.push('GPT returned no ad');
  if (fill === 'unknown') reasons.push('No fill observed');
  if (requests.some((cycle) => cycle.incompleteSequence))
    reasons.push('GPT event sequence incomplete');
  if (requests.some((cycle) => (cycle.trustedServerCreativeFailures?.length ?? 0) > 0))
    reasons.push('Creative bridge failure observed');
  return reasons;
}

function linkFor(
  runtimeSlotNumber: number,
  joined: TraceEvidenceView | undefined
): TraceSlotLink | undefined {
  const links = (joined?.auctions ?? []).flatMap((auction, index) =>
    auction.slots
      .filter(
        (entry) => entry.correlation === 'matched' && entry.runtimeSlotNumber === runtimeSlotNumber
      )
      .map((entry) => ({
        auctionNumber: index + 1,
        sourceLabel: auction.sourceLabel,
        serverSlotNumber: entry.serverSlot.slot_number,
        candidate: entry.serverSlot.candidate,
      }))
  );
  return links[0];
}

/**
 * Summarises one validated report for the trace page.
 *
 * Every sentence restates retained counts or classes. It never names an auction
 * winner, never compares server and browser clocks, and never invents thresholds.
 */
export function summarizeTraceReport(
  report: TraceReportV1,
  joined: TraceEvidenceView | undefined
): TraceSummary {
  const slots = report.gpt_diagnostics.slots.map((slot): TraceSlotCard => {
    const fill = fillOf(slot.requests);
    const cycle = representative(slot.requests);
    return {
      runtimeSlotNumber: slot.runtimeSlotNumber,
      fill,
      backfill: cycle?.isBackfill === true,
      size: cycle?.size,
      requestCount: slot.requests.length,
      timingRequestNumber: cycle?.requestNumber,
      timing: timing(cycle),
      link: linkFor(slot.runtimeSlotNumber, joined),
      attention: attentionFor(fill, slot.requests),
    };
  });
  const auctions = report.server_auctions;
  const calls = auctions.flatMap((auction) => auction.provider_calls);
  const errors = calls.filter((call) => call.status === 'error').length;
  const completed = auctions.filter((auction) => auction.terminal_status === 'completed').length;
  // Mediator responses echo bidder bids, so only bidder calls count as returned bids.
  const bids = calls
    .filter((call) => call.role === 'bidder')
    .reduce((sum, call) => sum + call.returned_bid_count, 0);
  const selected = auctions
    .flatMap((auction) => auction.slots)
    .filter((slot) => slot.candidate === 'selected').length;
  const count = (fill: TraceSlotFill): number => slots.filter((slot) => slot.fill === fill).length;
  const filled = count('filled');
  const linked = slots.filter((slot) => slot.link).length;

  const headline = slots.length
    ? `${filled} of ${plural(slots.length, 'ad slot', 'ad slots')} filled${
        auctions.length ? ` · ${plural(bids, 'Trusted Server bid', 'Trusted Server bids')}` : ''
      }`
    : 'No GPT ad slots observed';

  const reading: string[] = [];
  if (!auctions.length) reading.push('No server auction was captured for this page.');
  else if (completed === auctions.length)
    reading.push(
      auctions.length === 1
        ? 'The server auction completed.'
        : auctions.length === 2
          ? 'Both server auctions completed.'
          : `All ${auctions.length} server auctions completed.`
    );
  else {
    // Name every other recorded outcome so the reader is never left guessing.
    const outcomes = new Map<string, number>();
    for (const auction of auctions) {
      if (auction.terminal_status === 'completed') continue;
      const status = auction.terminal_status.replace(/_/g, ' ');
      const outcome = auction.terminal_reason
        ? `${status} (${auction.terminal_reason.replace(/_/g, ' ')})`
        : status;
      outcomes.set(outcome, (outcomes.get(outcome) ?? 0) + 1);
    }
    reading.push(
      `${completed} of ${auctions.length} server auctions completed; ${[...outcomes]
        .map(([outcome, total]) => `${total} ${outcome}`)
        .join(', ')}.`
    );
  }
  if (auctions.length)
    reading.push(
      bids
        ? `${plural(bids, 'bid was', 'bids were')} returned to Trusted Server.`
        : 'No bids were returned to Trusted Server.'
    );
  if (selected)
    reading.push(
      `Trusted Server selected a candidate for ${plural(selected, 'server slot', 'server slots')}.`
    );
  if (errors)
    reading.push(`${errors} of ${plural(calls.length, 'provider call', 'provider calls')} failed.`);
  const classes = new Map<string, number>();
  for (const slot of report.gpt_diagnostics.slots) {
    const cycle = representative(slot.requests);
    if (cycle?.isEmpty === false && cycle.responseClass)
      classes.set(cycle.responseClass, (classes.get(cycle.responseClass) ?? 0) + 1);
  }
  if (classes.size)
    reading.push(
      `GPT classified the filled ads as ${[...classes]
        .map(([name, total]) => `${total} ${name.replace(/_/g, ' ')}`)
        .join(' and ')}.`
    );
  if (auctions.length && slots.length && linked < slots.length)
    reading.push(
      `${slots.length - linked} of ${plural(slots.length, 'ad slot', 'ad slots')} could not be linked to a server auction.`
    );

  return {
    headline,
    reading,
    needsAttention:
      completed < auctions.length || errors > 0 || slots.some((slot) => slot.attention.length > 0),
    stats: [
      {
        value: auctions.length,
        label: auctions.length === 1 ? 'server auction' : 'server auctions',
      },
      { value: errors, label: errors === 1 ? 'provider error' : 'provider errors' },
      { value: linked, label: linked === 1 ? 'slot linked to TS' : 'slots linked to TS' },
    ],
    fills: (['filled', 'empty', 'unknown'] as const)
      .map((fill) => ({ fill, count: count(fill) }))
      .filter((entry) => entry.count > 0),
    slots,
  };
}
