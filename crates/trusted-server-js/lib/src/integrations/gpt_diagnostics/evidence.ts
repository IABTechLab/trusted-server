import type { GptDiagnosticsPrebidAuctionEvidence } from '../../core/types';

/** Return a mutable copy of retained Prebid auction evidence. */
export function clonePrebidAuctionEvidence(
  evidence: GptDiagnosticsPrebidAuctionEvidence
): GptDiagnosticsPrebidAuctionEvidence {
  return {
    ...evidence,
    ...(evidence.targetingCandidate
      ? { targetingCandidate: { ...evidence.targetingCandidate } }
      : {}),
    ...(evidence.win ? { win: { ...evidence.win } } : {}),
  };
}
