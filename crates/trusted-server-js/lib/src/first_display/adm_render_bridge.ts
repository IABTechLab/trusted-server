import type {
  FirstDisplayGptBoundCycleV1,
  FirstDisplayGptRenderResult,
} from './adapters/googletag';
import type { FirstDisplayRenderBridgeV1 } from './driver';

const ADM_SANDBOX =
  'allow-forms allow-popups allow-popups-to-escape-sandbox allow-scripts allow-top-navigation-by-user-activation';
const ADM_LOAD_DEADLINE_MS = 5_000;
const RESERVATION_TTL_MS = 15 * 60 * 1_000;
const MAX_CAPABILITIES = 320;
const MAX_U32 = 4_294_967_295;
const RESERVATION_ID = /^r1_[A-Za-z0-9_-]{22}$/;

export interface FirstDisplayAdmRenderBridgeOptionsV1 {
  readonly clearTimer: (handle: unknown) => void;
  readonly document: Document;
  readonly now: () => number;
  readonly onNativeMutation?: () => boolean;
  readonly setTimer: (callback: () => void, delayMs: number) => unknown;
}

interface AdmAttempt {
  readonly cycle: FirstDisplayGptBoundCycleV1;
  readonly expiresAtMs: number;
  readonly onTerminal: (
    result: 'accepted' | 'failed' | 'cancelled',
    reason: string | null
  ) => void;
  readonly ordinal: number;
  active: boolean;
  frame: HTMLIFrameElement | undefined;
  timer: unknown;
}

function validAdmCycle(cycle: FirstDisplayGptBoundCycleV1): boolean {
  try {
    return (
      cycle.bid.renderSource.type === 'adm' &&
      cycle.slotId === cycle.bid.slot &&
      cycle.slotId === cycle.placement.slot &&
      cycle.element.ownerDocument.getElementById(cycle.element.id) === cycle.element &&
      RESERVATION_ID.test(cycle.bid.rendererReservationId)
    );
  } catch {
    return false;
  }
}

function configureFrame(frame: HTMLIFrameElement, width: number, height: number): void {
  frame.setAttribute('sandbox', ADM_SANDBOX);
  frame.setAttribute('referrerpolicy', 'no-referrer');
  frame.setAttribute('width', String(width));
  frame.setAttribute('height', String(height));
  frame.setAttribute('scrolling', 'no');
  frame.setAttribute('frameborder', '0');
  frame.setAttribute('marginwidth', '0');
  frame.setAttribute('marginheight', '0');
  frame.setAttribute('title', 'Ad content');
  frame.setAttribute('aria-label', 'Advertisement');
  frame.setAttribute(
    'style',
    `border: 0; margin: 0; overflow: hidden; display: block; width: ${width}px; height: ${height}px;`
  );
}

/** Own only GPT-mediated ADM completion and the attributable empty-GAM fallback. */
export function createFirstDisplayAdmRenderBridge(
  options: FirstDisplayAdmRenderBridgeOptionsV1
): FirstDisplayRenderBridgeV1 {
  const attempts = new Map<string, AdmAttempt>();
  const committedFrames = new Map<
    string,
    Readonly<{ frame: HTMLIFrameElement; token: string }>
  >();
  const timers = new Set<unknown>();
  let nextReservationOrdinal = 1;
  let lastNow = Number.NEGATIVE_INFINITY;
  let sealed = false;
  let ingressClosed = false;
  let handoffCaptured = false;
  let committedArtifactsDetached = false;
  let disposed = false;

  const notifyNativeMutation = (): void => {
    try {
      options.onNativeMutation?.();
    } catch {
      // Observation cannot alter the terminal renderer state.
    }
  };
  const readNow = (): number | undefined => {
    try {
      const value = options.now();
      if (!Number.isFinite(value) || value < lastNow) return undefined;
      lastNow = value;
      return value;
    } catch {
      return undefined;
    }
  };
  const clearOwnedTimer = (handle: unknown): void => {
    if (handle === undefined || !timers.delete(handle)) return;
    try {
      options.clearTimer(handle);
    } catch {
      // The attempt latch remains authoritative.
    }
  };
  const arm = (callback: () => void, delayMs: number): unknown => {
    try {
      let handle: unknown;
      handle = options.setTimer(() => {
        if (!timers.delete(handle)) return;
        callback();
      }, delayMs);
      if (handle === undefined) return undefined;
      timers.add(handle);
      return handle;
    } catch {
      return undefined;
    }
  };
  const settle = (
    attempt: AdmAttempt,
    result: 'accepted' | 'failed' | 'cancelled',
    reason: string | null
  ): boolean => {
    if (!attempt.active) return false;
    attempt.active = false;
    clearOwnedTimer(attempt.timer);
    attempt.timer = undefined;
    const frame = attempt.frame;
    attempt.frame = undefined;
    if (result === 'accepted' && frame?.isConnected) {
      committedFrames.set(
        attempt.cycle.slotId,
        Object.freeze({ frame, token: attempt.cycle.bid.rendererReservationId })
      );
    } else if (frame) {
      try {
        frame.onload = null;
        frame.onerror = null;
        frame.remove();
      } catch {
        // The attempt no longer owns a failed frame.
      }
    }
    try {
      attempt.onTerminal(result, result === 'accepted' ? null : reason);
    } catch {
      // A terminal observer cannot restore renderer authority.
    }
    notifyNativeMutation();
    return true;
  };
  const fail = (attempt: AdmAttempt, reason: string): boolean =>
    settle(attempt, 'failed', reason);

  return Object.freeze({
    bind: (
      cycle: FirstDisplayGptBoundCycleV1,
      onTerminal: AdmAttempt['onTerminal']
    ): boolean => {
      const reservationId = cycle.bid.rendererReservationId;
      const observedAt = readNow();
      const ordinal = nextReservationOrdinal;
      if (
        disposed ||
        sealed ||
        ingressClosed ||
        typeof onTerminal !== 'function' ||
        !validAdmCycle(cycle) ||
        attempts.size >= MAX_CAPABILITIES ||
        attempts.has(reservationId) ||
        observedAt === undefined ||
        ordinal > MAX_U32
      ) {
        return false;
      }
      attempts.set(reservationId, {
        active: true,
        cycle,
        expiresAtMs: observedAt + RESERVATION_TTL_MS,
        frame: undefined,
        onTerminal,
        ordinal,
        timer: undefined,
      });
      nextReservationOrdinal += 1;
      return true;
    },
    recordGam: (
      cycle: FirstDisplayGptBoundCycleV1,
      result: FirstDisplayGptRenderResult
    ): boolean => {
      const attempt = attempts.get(cycle.bid.rendererReservationId);
      if (!attempt?.active || attempt.cycle !== cycle) return false;
      if (result === 'nonempty_gam') return settle(attempt, 'accepted', null);
      const source = cycle.bid.renderSource;
      if (source.type !== 'adm') return fail(attempt, 'winner_not_renderable');
      try {
        const frame = options.document.createElement('iframe');
        configureFrame(frame, source.width, source.height);
        const intended = `<!doctype html><html><head><meta charset="utf-8"><meta name="referrer" content="no-referrer"><style>html,body{border:0;margin:0;padding:0;overflow:hidden}</style></head><body>${source.adm}</body></html>`;
        frame.onload = () => {
          if (
            attempt.active &&
            attempt.frame === frame &&
            frame.parentNode === cycle.element &&
            frame.srcdoc === intended &&
            frame.getAttribute('src') === null
          ) {
            settle(attempt, 'accepted', null);
          }
        };
        frame.onerror = () => fail(attempt, 'adm_document_no_load');
        frame.srcdoc = intended;
        attempt.frame = frame;
        attempt.timer = arm(
          () => fail(attempt, 'adm_document_no_load'),
          ADM_LOAD_DEADLINE_MS
        );
        if (attempt.timer === undefined) return fail(attempt, 'internal_error');
        cycle.element.appendChild(frame);
        return true;
      } catch {
        return fail(attempt, 'adm_document_no_load');
      }
    },
    recordFailure: (cycle: FirstDisplayGptBoundCycleV1): boolean => {
      const attempt = attempts.get(cycle.bid.rendererReservationId);
      return Boolean(attempt?.active && attempt.cycle === cycle && fail(attempt, 'gpt_request_failed'));
    },
    sealTsAdmission: (): void => {
      if (disposed || sealed || [...attempts.values()].some(({ active }) => active)) {
        throw new TypeError('tsjs');
      }
      sealed = true;
    },
    closeIngress: (): boolean => {
      if (disposed || ingressClosed || !sealed) return false;
      ingressClosed = true;
      for (const handle of [...timers]) clearOwnedTimer(handle);
      return true;
    },
    captureHandoff: () => {
      if (disposed || !ingressClosed || handoffCaptured) return undefined;
      const observedAt = readNow();
      if (observedAt === undefined) return undefined;
      handoffCaptured = true;
      return Object.freeze({
        artifacts: Object.freeze(
          [...committedFrames.entries()].map(([slotId, { frame, token }]) =>
            Object.freeze({
              identity: frame,
              kind: 'gpt_adm' as const,
              owner: 'trusted_server' as const,
              slotId,
              token,
            })
          )
        ),
        clockEpochMs: observedAt,
        nextReservationOrdinal,
        nextTicketOrdinal: 1,
        tombstones: Object.freeze(
          [...attempts.entries()]
            .filter(([, attempt]) => !attempt.active && attempt.expiresAtMs > observedAt)
            .map(([value, attempt]) =>
              Object.freeze({
                kind: 'reservation' as const,
                value,
                expiresAtMs: attempt.expiresAtMs,
                ordinal: attempt.ordinal,
              })
            )
        ),
      });
    },
    detachCommittedArtifacts: (): boolean => {
      if (disposed || !ingressClosed || !handoffCaptured || committedArtifactsDetached) {
        return false;
      }
      committedArtifactsDetached = true;
      return true;
    },
    dispose: (): void => {
      if (disposed) return;
      disposed = true;
      for (const attempt of attempts.values()) {
        if (attempt.active) settle(attempt, 'cancelled', 'navigation_disposed');
      }
      for (const handle of [...timers]) clearOwnedTimer(handle);
      if (!committedArtifactsDetached) {
        for (const { frame } of committedFrames.values()) {
          try {
            frame.remove();
          } catch {
            // A terminal owner cannot regain authority through a retained node.
          }
        }
      }
      committedFrames.clear();
      attempts.clear();
    },
  });
}
