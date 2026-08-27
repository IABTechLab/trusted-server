import { resolveSlotElementByDivId } from './slot_element';
import type {
  FirstImpressionPhase,
  FirstImpressionPublisherAuction,
  FirstImpressionSlotClaim,
  FirstImpressionState,
  TsjsApi,
} from './types';

/** Time allowed for one navigation's losing first-impression delivery. */
export const FIRST_IMPRESSION_LEASE_MS = 5000;

const MAX_FIRST_IMPRESSION_SLOTS = 256;
const MAX_PUBLISHER_AUCTIONS_PER_SLOT = 16;

function currentGeneration(ts: TsjsApi): number {
  return ts.navGeneration ?? 0;
}

function claimMatchesElement(
  claim: FirstImpressionSlotClaim,
  element: HTMLElement,
  generation: number
): boolean {
  return (
    claim.generation === generation &&
    claim.slotElementId === element.id &&
    claim.element === element &&
    element.ownerDocument === document &&
    element.isConnected &&
    document.getElementById(element.id) === element
  );
}

function removePublisherAuction(
  state: FirstImpressionState,
  claim: FirstImpressionSlotClaim,
  token: string,
  now: number
): void {
  delete claim.publisherAuctions[token];
  if (
    claim.owner === 'publisher' &&
    (claim.phase === 'auctioning' || claim.phase === 'delivery_pending') &&
    Object.keys(claim.publisherAuctions).length === 0 &&
    claim.expiresAt <= now
  ) {
    delete state.slots[claim.slotElementId];
  }
}

function pruneFirstImpressionState(ts: TsjsApi, now = Date.now()): FirstImpressionState {
  const generation = currentGeneration(ts);
  if (ts.firstImpression?.generation !== generation) {
    ts.firstImpression = { generation, nextToken: 0, slots: {}, fallbackSlots: {} };
  }

  const state = ts.firstImpression;
  state.slots ??= {};
  state.fallbackSlots ??= {};
  for (const [elementId, claim] of Object.entries(state.slots)) {
    if (
      claim.slotElementId !== elementId ||
      !claimMatchesElement(claim, claim.element, generation)
    ) {
      delete state.slots[elementId];
      continue;
    }
    const hasReservedFallback =
      claim.owner === 'publisher' &&
      (claim.phase === 'auctioning' || claim.phase === 'delivery_pending') &&
      state.fallbackSlots[elementId] === claim.element;
    for (const [token, auction] of Object.entries(claim.publisherAuctions)) {
      // A TS-owned losing publisher auction remains a fail-closed tombstone for
      // this physical element and navigation. Publisher registrations also stay
      // intact while an expired claim is waiting to transition to its reserved
      // TS fallback, so an overlapping late callback cannot escape suppression.
      if (
        auction.expiresAt <= now &&
        !hasReservedFallback &&
        !(claim.owner === 'trusted_server' && auction.suppressDelivery)
      ) {
        removePublisherAuction(state, claim, token, now);
      }
    }
    if (
      claim.owner === 'publisher' &&
      (claim.phase === 'auctioning' || claim.phase === 'delivery_pending') &&
      Object.keys(claim.publisherAuctions).length === 0 &&
      claim.expiresAt <= now &&
      !hasReservedFallback
    ) {
      delete state.slots[elementId];
    }
  }
  for (const [elementId, element] of Object.entries(state.fallbackSlots)) {
    if (
      !element.isConnected ||
      element.id !== elementId ||
      document.getElementById(elementId) !== element
    ) {
      delete state.fallbackSlots[elementId];
    }
  }
  return state;
}

/** Resolve a publisher ad-unit code with the same contract GPT uses. */
export function resolveFirstImpressionElement(adUnitCode: string): HTMLElement | undefined {
  return resolveSlotElementByDivId(adUnitCode).element ?? undefined;
}

/** Return the live ownership claim for an exact slot element. */
export function firstImpressionClaim(
  ts: TsjsApi,
  element: HTMLElement
): FirstImpressionSlotClaim | undefined {
  const state = pruneFirstImpressionState(ts);
  const claim = state.slots[element.id];
  return claim && claimMatchesElement(claim, element, state.generation) ? claim : undefined;
}

function storeClaim(state: FirstImpressionState, claim: FirstImpressionSlotClaim): boolean {
  if (
    !state.slots[claim.slotElementId] &&
    Object.keys(state.slots).length >= MAX_FIRST_IMPRESSION_SLOTS
  ) {
    return false;
  }
  state.slots[claim.slotElementId] = claim;
  return true;
}

/** Atomically claim an untouched slot for Trusted Server. */
export function claimFirstImpressionForTrustedServer(
  ts: TsjsApi,
  element: HTMLElement,
  now = Date.now()
): FirstImpressionSlotClaim | undefined {
  const state = pruneFirstImpressionState(ts, now);
  const existing = state.slots[element.id];
  if (existing && claimMatchesElement(existing, element, state.generation)) {
    const canTransitionPublisherFallback =
      existing.owner === 'publisher' &&
      existing.phase !== 'requested' &&
      existing.phase !== 'rendered' &&
      existing.expiresAt <= now &&
      state.fallbackSlots[element.id] === element;
    if (!canTransitionPublisherFallback) return undefined;

    existing.owner = 'trusted_server';
    existing.phase = 'delivery_pending';
    existing.expiresAt = now + FIRST_IMPRESSION_LEASE_MS;
    for (const auction of Object.values(existing.publisherAuctions)) {
      auction.suppressDelivery = true;
    }
    return existing;
  }

  const claim: FirstImpressionSlotClaim = {
    generation: state.generation,
    slotElementId: element.id,
    element,
    owner: 'trusted_server',
    phase: 'delivery_pending',
    expiresAt: now + FIRST_IMPRESSION_LEASE_MS,
    publisherAuctions: {},
  };
  return storeClaim(state, claim) ? claim : undefined;
}

function schedulePublisherAuctionExpiry(ts: TsjsApi, token: string): void {
  window.setTimeout(() => {
    // Pruning releases ordinary publisher claims. TS-owned suppression tokens
    // deliberately survive as bounded tombstones until navigation/element change.
    findPublisherAuction(ts, token);
  }, FIRST_IMPRESSION_LEASE_MS);
}

/** Release a TS claim when slot setup failed before any request could start. */
export function releaseTrustedServerFirstImpressionClaim(
  ts: TsjsApi,
  element: HTMLElement,
  claim: FirstImpressionSlotClaim
): void {
  const state = pruneFirstImpressionState(ts);
  if (
    state.slots[element.id] === claim &&
    claim.owner === 'trusted_server' &&
    claim.phase === 'delivery_pending'
  ) {
    delete state.slots[element.id];
    if (state.fallbackSlots[element.id] === element) {
      delete state.fallbackSlots[element.id];
    }
  }
}

/** Register real publisher auctions before native `requestBids()` starts. */
export function registerPublisherFirstImpressionAuctions(
  ts: TsjsApi,
  adUnitCodes: Iterable<string>,
  now = Date.now()
): Map<string, string> {
  const state = pruneFirstImpressionState(ts, now);
  const registrations = new Map<string, string>();

  for (const adUnitCode of adUnitCodes) {
    const element = resolveFirstImpressionElement(adUnitCode);
    if (!element) continue;

    let claim = state.slots[element.id];
    if (!claim || !claimMatchesElement(claim, element, state.generation)) {
      claim = {
        generation: state.generation,
        slotElementId: element.id,
        element,
        owner: 'publisher',
        phase: 'auctioning',
        expiresAt: now + FIRST_IMPRESSION_LEASE_MS,
        publisherAuctions: {},
      };
      if (!storeClaim(state, claim)) continue;
    }

    if (
      claim.owner === 'publisher' &&
      (claim.phase === 'requested' || claim.phase === 'rendered')
    ) {
      continue;
    }
    if (
      claim.owner === 'trusted_server' &&
      (claim.publisherRegistrationClosed || claim.expiresAt <= now)
    ) {
      continue;
    }
    if (Object.keys(claim.publisherAuctions).length >= MAX_PUBLISHER_AUCTIONS_PER_SLOT) continue;

    const token = `${state.generation}:${++state.nextToken}`;
    const auction: FirstImpressionPublisherAuction = {
      token,
      adUnitCode,
      phase: 'auctioning',
      expiresAt: now + FIRST_IMPRESSION_LEASE_MS,
      adIds: [],
      suppressDelivery: claim.owner === 'trusted_server',
    };
    claim.publisherAuctions[token] = auction;
    if (claim.owner === 'publisher') claim.expiresAt = Math.max(claim.expiresAt, auction.expiresAt);
    registrations.set(adUnitCode, token);
    schedulePublisherAuctionExpiry(ts, token);
  }

  return registrations;
}

function findPublisherAuction(
  ts: TsjsApi,
  token: string,
  now = Date.now()
):
  | {
      state: FirstImpressionState;
      claim: FirstImpressionSlotClaim;
      auction: FirstImpressionPublisherAuction;
    }
  | undefined {
  const state = pruneFirstImpressionState(ts, now);
  for (const claim of Object.values(state.slots)) {
    const auction = claim.publisherAuctions[token];
    if (auction) return { state, claim, auction };
  }
  return undefined;
}

/** Move one publisher auction to delivery-pending without disturbing overlaps. */
export function markPublisherFirstImpressionDeliveryPending(
  ts: TsjsApi,
  token: string,
  adIds: string[],
  now = Date.now()
): void {
  const found = findPublisherAuction(ts, token, now);
  if (!found) return;
  found.auction.phase = 'delivery_pending';
  found.auction.adIds = [...new Set(adIds)];
  if (found.claim.owner === 'publisher') found.claim.phase = 'delivery_pending';
}

/** Release exactly one publisher auction token after failure, timeout, or removal. */
export function releasePublisherFirstImpressionAuction(
  ts: TsjsApi,
  token: string,
  now = Date.now()
): void {
  const found = findPublisherAuction(ts, token, now);
  if (!found) return;
  if (found.claim.owner === 'trusted_server' && found.auction.suppressDelivery) {
    found.claim.publisherRegistrationClosed = true;
    return;
  }
  found.auction.expiresAt = Math.min(found.auction.expiresAt, now);
  if (
    found.claim.owner === 'publisher' &&
    Object.keys(found.claim.publisherAuctions).length === 1
  ) {
    found.claim.expiresAt = now;
  }
  removePublisherAuction(found.state, found.claim, token, now);
}

/** Consume one correlated publisher delivery and report whether TS owns it. */
export function consumePublisherFirstImpressionDelivery(
  ts: TsjsApi,
  token: string | undefined,
  now = Date.now()
): boolean {
  if (!token) return false;
  const found = findPublisherAuction(ts, token, now);
  if (!found) return false;

  const suppress = found.claim.owner === 'trusted_server' && found.auction.suppressDelivery;
  delete found.claim.publisherAuctions[token];
  if (suppress) found.claim.publisherRegistrationClosed = true;
  return suppress;
}

/** Record a GPT request or render, using publisher ownership when no claimant exists. */
export function observeFirstImpressionGptLifecycle(
  ts: TsjsApi,
  element: HTMLElement,
  phase: Extract<FirstImpressionPhase, 'requested' | 'rendered'>,
  now = Date.now()
): void {
  const state = pruneFirstImpressionState(ts, now);
  let claim = state.slots[element.id];
  if (!claim || !claimMatchesElement(claim, element, state.generation)) {
    claim = {
      generation: state.generation,
      slotElementId: element.id,
      element,
      owner: 'publisher',
      phase,
      expiresAt: Number.POSITIVE_INFINITY,
      publisherAuctions: {},
    };
    storeClaim(state, claim);
    return;
  }

  claim.phase = phase;
  if (claim.owner === 'publisher') {
    claim.expiresAt = Number.POSITIVE_INFINITY;
  } else {
    // Once TS has committed a GPT request, only publisher auctions that were
    // already registered can still represent an overlapping first impression.
    // New publisher refreshes are ordinary later impressions and must proceed.
    claim.publisherRegistrationClosed = true;
  }
}

/** Reserve the only Trusted Server fallback allowed for this physical slot and generation. */
export function reservePublisherFirstImpressionFallback(
  ts: TsjsApi,
  element: HTMLElement
): boolean {
  const state = pruneFirstImpressionState(ts);
  const reservedElement = state.fallbackSlots[element.id];
  if (reservedElement) return false;
  state.fallbackSlots[element.id] = element;
  return true;
}

/** Delay before an abandoned publisher claim can receive one per-slot TS fallback. */
export function publisherFirstImpressionRetryDelay(
  ts: TsjsApi,
  element: HTMLElement,
  now = Date.now()
): number | undefined {
  const claim = firstImpressionClaim(ts, element);
  if (!claim) return 0;
  if (claim.owner !== 'publisher') return undefined;
  if (claim.phase === 'requested' || claim.phase === 'rendered') return undefined;
  return Math.max(0, claim.expiresAt - now);
}
