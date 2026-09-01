import type { GptDiagnosticsTrustedServerOpportunity, Size } from '../core/types';

/** Opaque GPT slot identity safe to pass through the data-only diagnostics capability. */
export interface GptDiagnosticsSlotIdentityV1 {
  readonly token: object;
  readonly traceToken?: string | undefined;
  readonly runtimeSlotNumber?: number | undefined;
  readonly cycleOrdinal?: number | undefined;
  readonly elementId?: string | undefined;
  readonly adUnitPath?: string | undefined;
}

/** Release-private Trusted Server request intent consumed only by GPT diagnostics. */
export interface GptDiagnosticsOpportunityFact {
  readonly kind: 'trustedServerOpportunity';
  readonly auctionSlotId: string;
  readonly opportunity: GptDiagnosticsTrustedServerOpportunity;
  readonly requestedSlotSizes: readonly Readonly<Size>[];
  readonly slot: Readonly<GptDiagnosticsSlotIdentityV1>;
  readonly trustedServerAuctionId?: string | undefined;
}

const opportunityFacts = new WeakSet<object>();

/** Copy one immutable request-intent fact before its exact GPT request starts. */
export function createTrustedServerOpportunityFact(input: {
  readonly auctionSlotId: string;
  readonly opportunity: GptDiagnosticsTrustedServerOpportunity;
  readonly requestedSlotSizes: readonly Readonly<Size>[];
  readonly slot: Readonly<GptDiagnosticsSlotIdentityV1>;
  readonly trustedServerAuctionId?: string | undefined;
}): Readonly<GptDiagnosticsOpportunityFact> | undefined {
  try {
    if (
      typeof input.auctionSlotId !== 'string' ||
      input.auctionSlotId.length === 0 ||
      input.auctionSlotId.length > 256 ||
      (input.opportunity !== 'renderable_candidate' &&
        input.opportunity !== 'unrenderable_candidate' &&
        input.opportunity !== 'no_candidate') ||
      typeof input.slot !== 'object' ||
      input.slot === null ||
      !Object.isFrozen(input.slot) ||
      !Array.isArray(input.requestedSlotSizes)
    ) {
      return undefined;
    }
    const requestedSlotSizes: Readonly<Size>[] = [];
    for (let index = 0; index < input.requestedSlotSizes.length && index < 16; index += 1) {
      const size = input.requestedSlotSizes[index];
      if (
        !Array.isArray(size) ||
        size.length !== 2 ||
        !Number.isInteger(size[0]) ||
        !Number.isInteger(size[1]) ||
        (size[0] ?? 0) < 1 ||
        (size[0] ?? 0) > 4_096 ||
        (size[1] ?? 0) < 1 ||
        (size[1] ?? 0) > 4_096
      ) {
        continue;
      }
      requestedSlotSizes.push(Object.freeze([size[0], size[1]] as Size));
    }
    if (requestedSlotSizes.length === 0) return undefined;
    if (
      input.trustedServerAuctionId !== undefined &&
      (typeof input.trustedServerAuctionId !== 'string' ||
        input.trustedServerAuctionId.length === 0 ||
        input.trustedServerAuctionId.length > 256)
    ) {
      return undefined;
    }
    const fact = Object.freeze({
      kind: 'trustedServerOpportunity' as const,
      auctionSlotId: input.auctionSlotId,
      opportunity: input.opportunity,
      requestedSlotSizes: Object.freeze(requestedSlotSizes),
      slot: input.slot,
      ...(input.trustedServerAuctionId === undefined
        ? {}
        : { trustedServerAuctionId: input.trustedServerAuctionId }),
    });
    opportunityFacts.add(fact);
    return fact;
  } catch {
    return undefined;
  }
}

/** Verify that an opportunity fact was minted by this release's private producer. */
export function isTrustedServerOpportunityFact(
  candidate: unknown
): candidate is Readonly<GptDiagnosticsOpportunityFact> {
  return typeof candidate === 'object' && candidate !== null && opportunityFacts.has(candidate);
}
