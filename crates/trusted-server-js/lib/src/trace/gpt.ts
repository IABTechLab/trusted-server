import type { AuctionSlot } from '../core/types';

import type { TraceCollector } from './collector';
import type { TraceGptIdentity } from './types';
import { traceItems } from './shape';
import {
  parseTraceAuctionTransport,
  validDiagnosticAuctionId,
  validTraceSlotRef,
} from './validation';

/** Ordinary marker plus the optional exact identity for one concrete GPT opportunity. */
export interface TraceGptOpportunity {
  readonly auctionId: string | undefined;
  readonly identity?: TraceGptIdentity;
}

/** Narrow internal facade for the shared validated SSAT/SPA slot bindings. */
export interface TraceGptBridge {
  observeTransport(
    slots: readonly AuctionSlot[] | undefined,
    value: unknown,
    source: 'initial_navigation_ssat' | 'spa_page_bids'
  ): void;
  observePageBids(response: unknown, slots: readonly AuctionSlot[]): void;
  identity(slot: AuctionSlot): TraceGptIdentity | undefined;
  opportunity(slot: AuctionSlot, auctionId: string | undefined): TraceGptOpportunity;
}

function ownData(value: unknown, key: string): unknown {
  if (value === null || typeof value !== 'object') return undefined;
  const property = Object.getOwnPropertyDescriptor(value, key);
  return property && Object.prototype.hasOwnProperty.call(property, 'value')
    ? property.value
    : undefined;
}

/** Creates the active core bridge shared by bootstrap and independent GPT bundles. */
export function createTraceGptBridge(collector: TraceCollector): TraceGptBridge {
  let identities: WeakMap<object, TraceGptIdentity> | undefined;
  function observeTraceGptTransport(
    slots: readonly AuctionSlot[] | undefined,
    value: unknown,
    source: 'initial_navigation_ssat' | 'spa_page_bids'
  ): void {
    try {
      // A new accepted document/navigation batch replaces slot bindings only.
      // Previously consumed store intents and accumulated capture stay intact.
      identities = undefined;
      if (value === undefined) return;
      const transport = parseTraceAuctionTransport(value);
      if (!transport || (transport.evidence && transport.evidence.source !== source)) {
        collector.recordTransport(null);
        return;
      }
      collector.recordTransport(transport);
      if (!transport.evidence) return;
      // Correlation failures cannot discard already validated server evidence.
      // Read only own array entries and the sole allowed extension member.
      let delivered: unknown[] | undefined;
      try {
        delivered = slots === undefined ? [] : traceItems(slots, 64);
      } catch {
        delivered = undefined;
      }
      if (!delivered) {
        collector.recordInterpretationIssue('correlation_unavailable');
        return;
      }
      const evidenceCounts = new Map<string, number>();
      for (const slot of transport.evidence.slots)
        evidenceCounts.set(slot.slot_ref, (evidenceCounts.get(slot.slot_ref) ?? 0) + 1);
      const deliveredCounts = new Map<string, number>();
      const refs = delivered.map((slot) => {
        try {
          const ref = ownData(ownData(ownData(slot, 'ext'), 'trusted_server'), 'trace_slot_ref');
          if (!validTraceSlotRef(ref)) return undefined;
          deliveredCounts.set(ref, (deliveredCounts.get(ref) ?? 0) + 1);
          return ref;
        } catch {
          return undefined;
        }
      });
      for (const [index, slot] of delivered.entries()) {
        const ref = refs[index];
        if (!ref || deliveredCounts.get(ref) !== 1 || evidenceCounts.get(ref) !== 1) {
          collector.recordInterpretationIssue('correlation_unavailable');
          continue;
        }
        identities ??= new WeakMap();
        identities.set(
          slot as object,
          Object.freeze({
            diagnostic_auction_id: transport.evidence.diagnostic_auction_id,
            slot_ref: ref,
          })
        );
      }
    } catch {
      // Diagnostic parsing never changes ordinary slot or bid delivery.
    }
  }

  /** Reads the sole optional SPA member after the navigation's existing guards. */
  function observeTraceGptPageBids(response: unknown, slots: readonly AuctionSlot[]): void {
    if (response === null || typeof response !== 'object') return;
    let value: unknown;
    try {
      const property = Object.getOwnPropertyDescriptor(response, 'trace_auction');
      value =
        property && !Object.prototype.hasOwnProperty.call(property, 'value')
          ? null
          : property?.value;
    } catch {
      value = null;
    }
    observeTraceGptTransport(slots, value, 'spa_page_bids');
  }

  /** Returns only the validated identity bound to this exact delivered slot object. */
  return Object.freeze({
    observeTransport: observeTraceGptTransport,
    observePageBids: observeTraceGptPageBids,
    identity: (slot: AuctionSlot) => identities?.get(slot),
    opportunity: (slot: AuctionSlot, auctionId: string | undefined): TraceGptOpportunity => {
      const identity = identities?.get(slot);
      if (!identity) return { auctionId };
      // Only an absent marker can inherit the validated pre-dispatch token.
      // Provided markers keep the store's existing normalization and semantics.
      if (auctionId === undefined) return { auctionId: identity.diagnostic_auction_id, identity };
      const normalized = typeof auctionId === 'string' ? auctionId.trim() : undefined;
      if (validDiagnosticAuctionId(normalized) && normalized === identity.diagnostic_auction_id)
        return { auctionId, identity };
      collector.recordInterpretationIssue('correlation_unavailable');
      return { auctionId };
    },
  });
}
