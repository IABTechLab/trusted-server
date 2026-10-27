import type {
  GptDiagnosticsAuctionWinner,
  GptDiagnosticsRecorder,
  GptDiagnosticsSlotHandle,
  TsjsApi,
} from '../../core/types';

type DiagnosticSlot = GptDiagnosticsSlotHandle & {
  getTargeting?: (key: string) => string[];
};
type DiagnosticPrebid = Parameters<
  NonNullable<GptDiagnosticsRecorder['recordCompletedPrebidAuction']>
>[0];
type DiagnosticWindow = {
  performance: Pick<Performance, 'now'>;
  tsjs?: Pick<TsjsApi, 'navGeneration'>;
};

const MAX_PREBID_DIAGNOSTIC_ATTEMPTS = 128;
const PREBID_DIAGNOSTIC_WINDOW_MS = 30_000;

interface PrebidDiagnosticAttempt {
  prebid: DiagnosticPrebid;
  slot: DiagnosticSlot;
  generation: number;
  expiresAtMs: number;
}

function prebidDiagnosticKey(auctionId: string, adUnitCode: string): string {
  return `${auctionId}\u0000${adUnitCode}`;
}

function boundedTargetingValue(
  slot: DiagnosticSlot,
  key: string,
  maxBytes: number
): string | undefined {
  let values: string[] | undefined;
  try {
    values = slot.getTargeting?.(key);
  } catch {
    return undefined;
  }
  if (!Array.isArray(values) || values.length !== 1) return undefined;
  const value = values[0]?.trim();
  if (!value || new TextEncoder().encode(value).length > maxBytes) return undefined;
  return value;
}

function targetingCandidate(slot: DiagnosticSlot): GptDiagnosticsAuctionWinner | undefined {
  const bidder = boundedTargetingValue(slot, 'hb_bidder', 128);
  const priceBucket = boundedTargetingValue(slot, 'hb_pb', 64);
  if (!bidder || !priceBucket || !/^\d+(?:\.\d+)?$/.test(priceBucket)) return undefined;
  return { bidder, priceBucket };
}

/** Correlates targeting and bidWon only in the optional diagnostics bundle. */
export class GptDiagnosticsPrebidRecorder {
  private readonly attempts = new Map<string, PrebidDiagnosticAttempt>();
  private readonly installed = new Map<DiagnosticPrebid, (bid: unknown) => void>();
  private destroyed = false;

  constructor(
    private readonly target: DiagnosticWindow,
    private recorder:
      | Pick<GptDiagnosticsRecorder, 'recordPrebidAuction' | 'recordPrebidWin'>
      | undefined
  ) {}

  destroy(): void {
    if (this.destroyed) return;
    this.destroyed = true;
    this.recorder = undefined;
    this.attempts.clear();
    for (const [prebid, listener] of this.installed) {
      try {
        prebid.offEvent?.('bidWon', listener);
      } catch {
        // A missing or failing unsubscribe leaves an inert listener, not the store.
      }
    }
    this.installed.clear();
  }

  recordCompletedPrebidAuction = (
    pbjs: DiagnosticPrebid,
    rawAuctionId: unknown,
    auctionSlots: DiagnosticSlot[],
    adUnitCodes: string[]
  ): void => {
    if (this.destroyed) return;
    const recorder = this.recorder;
    if (!recorder || typeof rawAuctionId !== 'string') return;
    const auctionId = rawAuctionId;
    if (
      !auctionId ||
      auctionId !== auctionId.trim() ||
      new TextEncoder().encode(auctionId).length > 256
    )
      return;
    if (auctionSlots.length !== adUnitCodes.length) return;
    this.installWinListener(pbjs);
    const counts = new Map<string, number>();
    for (const code of adUnitCodes) counts.set(code, (counts.get(code) ?? 0) + 1);
    const nowMs = this.target.performance.now();
    for (const [key, attempt] of this.attempts) {
      if (nowMs > attempt.expiresAtMs) this.attempts.delete(key);
    }
    const generation = this.target.tsjs?.navGeneration ?? 0;
    for (let index = 0; index < auctionSlots.length; index += 1) {
      const slot = auctionSlots[index];
      const code = adUnitCodes[index];
      if (!slot || !code || counts.get(code) !== 1) continue;
      try {
        recorder.recordPrebidAuction(slot, auctionId, targetingCandidate(slot));
      } catch {
        // Diagnostics must not suppress the GAM request.
      }
      const key = prebidDiagnosticKey(auctionId, code);
      this.attempts.delete(key);
      this.attempts.set(key, {
        prebid: pbjs,
        slot,
        generation,
        expiresAtMs: nowMs + PREBID_DIAGNOSTIC_WINDOW_MS,
      });
      while (this.attempts.size > MAX_PREBID_DIAGNOSTIC_ATTEMPTS) {
        const oldest = this.attempts.keys().next().value;
        if (oldest === undefined) break;
        this.attempts.delete(oldest);
      }
    }
  };

  private installWinListener(pbjs: DiagnosticPrebid): void {
    if (this.installed.has(pbjs) || typeof pbjs.onEvent !== 'function') return;
    const listener = (rawBid: unknown): void => {
      if (this.destroyed) return;
      if (typeof rawBid !== 'object' || rawBid === null) return;
      const bid = rawBid as Record<string, unknown>;
      const auctionId = typeof bid.auctionId === 'string' ? bid.auctionId : undefined;
      const adUnitCode = typeof bid.adUnitCode === 'string' ? bid.adUnitCode : undefined;
      if (
        !auctionId ||
        !adUnitCode ||
        (bid.latestTargetedAuctionId !== undefined && bid.latestTargetedAuctionId !== auctionId)
      )
        return;
      const key = prebidDiagnosticKey(auctionId, adUnitCode);
      const attempt = this.attempts.get(key);
      if (attempt?.prebid !== pbjs) return;
      this.attempts.delete(key);
      if (
        this.target.performance.now() > attempt.expiresAtMs ||
        (this.target.tsjs?.navGeneration ?? 0) !== attempt.generation
      ) {
        return;
      }
      const adserverTargeting =
        typeof bid.adserverTargeting === 'object' && bid.adserverTargeting !== null
          ? (bid.adserverTargeting as Record<string, unknown>)
          : {};
      const winner = targetingCandidate({
        getTargeting: (targetingKey) => {
          const value = adserverTargeting[targetingKey];
          return typeof value === 'string' ? [value] : [];
        },
      });
      if (!winner) return;
      try {
        this.recorder?.recordPrebidWin(attempt.slot, auctionId, winner);
      } catch {
        // Diagnostics must not alter delivery.
      }
    };
    pbjs.onEvent('bidWon', listener);
    this.installed.set(pbjs, listener);
  }
}
