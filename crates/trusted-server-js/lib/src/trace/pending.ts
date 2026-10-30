import type { TraceCollector } from './collector';
import { observeTraceApiResponse, observeTraceApiFailure } from './runtime';

export interface TracePendingCarry {
  readonly collector: TraceCollector;
  readonly slotRefs: readonly string[];
}
export interface TraceBidBinding {
  readonly bidId: string;
  readonly bidderRequestId?: string;
  readonly slotRef?: string;
}
export interface TracePendingHandle {
  readonly key: symbol;
}
export interface TracePendingOptions {
  now?: () => number;
  timeout?: () => unknown;
  schedule?: (callback: () => void, delay: number) => ReturnType<typeof setTimeout>;
  cancel?: (timer: ReturnType<typeof setTimeout>) => void;
}
export interface TracePending {
  /** Undefined bindings retain exact responses while declining uninspectable ID hooks. */
  add(
    bindings: readonly TraceBidBinding[] | undefined,
    carry: TracePendingCarry
  ): TracePendingHandle | undefined;
  response(handle: TracePendingHandle | undefined, body: unknown): void;
  failure(bids: readonly Pick<TraceBidBinding, 'bidId' | 'bidderRequestId'>[]): void;
  clear(): void;
  destroy(): void;
}
const MAX_TIMEOUT = 2 ** 31 - 1 - 5000;
const MAX_PENDING = 128;
const MAX_BINDINGS = 2048;

function hookKey(bid: Pick<TraceBidBinding, 'bidId' | 'bidderRequestId'>): string | undefined {
  const requestId = bid.bidderRequestId;
  const id = bid.bidId;
  if (
    typeof requestId !== 'string' ||
    !requestId.length ||
    requestId.length > 128 ||
    typeof id !== 'string' ||
    !id.length ||
    id.length > 128
  )
    return undefined;
  // Length framing keeps the two SDK identities distinct without delimiter assumptions.
  return `${requestId.length}:${requestId}${id}`;
}

/** Checks the captured browser timeout and wall-clock expiry before retaining state. */
export function pendingExpiry(createdAtMs: number, configuredTimeout: unknown): number | undefined {
  if (!Number.isSafeInteger(createdAtMs) || createdAtMs < 0) return undefined;
  const timeout =
    typeof configuredTimeout === 'number' &&
    Number.isInteger(configuredTimeout) &&
    configuredTimeout >= 0 &&
    configuredTimeout <= MAX_TIMEOUT
      ? configuredTimeout
      : 3000;
  const expiresAtMs = createdAtMs + timeout + 5000;
  return Number.isSafeInteger(expiresAtMs) ? expiresAtMs : undefined;
}
interface PendingRecord {
  readonly bindings: readonly TraceBidBinding[];
  readonly carry: TracePendingCarry;
  readonly expiresAtMs: number;
  readonly timer: ReturnType<typeof setTimeout>;
  readonly unknownIds: boolean;
  hookEligible: boolean;
}

/** Owns bounded request-local tokens and retires markers before diagnostic callbacks. */
export function createTracePending(options: TracePendingOptions = {}): TracePending {
  const now = options.now ?? (() => Date.now());
  const schedule = options.schedule ?? ((callback, delay) => setTimeout(callback, delay));
  const cancel = options.cancel ?? ((timer) => clearTimeout(timer));
  const records = new Map<TracePendingHandle, PendingRecord>();
  const originalIds = new Map<string, Set<TracePendingHandle>>();
  let destroyed = false;
  function remove(handle: TracePendingHandle | undefined): PendingRecord | undefined {
    if (!handle) return undefined;
    const record = records.get(handle);
    if (!record) return undefined;
    records.delete(handle);
    for (const binding of record.bindings) {
      const key = hookKey(binding)!;
      const owners = originalIds.get(key);
      owners?.delete(handle);
      if (!owners?.size) originalIds.delete(key);
    }
    try {
      cancel(record.timer);
    } catch {
      /* Marker retirement precedes timer cleanup. */
    }
    return record;
  }
  function take(handle: TracePendingHandle | undefined): TracePendingCarry | undefined {
    const record = remove(handle);
    if (!record) return undefined;
    try {
      const current = now();
      return Number.isSafeInteger(current) && current >= 0 && current < record.expiresAtMs
        ? record.carry
        : undefined;
    } catch {
      return undefined;
    }
  }
  function clear(): void {
    for (const handle of records.keys()) remove(handle);
  }
  function noteUnavailable(record: PendingRecord): void {
    try {
      record.carry.collector.recordInterpretationIssue('correlation_unavailable');
    } catch {
      /* A diagnostic callback cannot alter request delivery. */
    }
  }
  return Object.freeze({
    add(bindings: readonly TraceBidBinding[] | undefined, carry: TracePendingCarry) {
      if (destroyed) return undefined;
      try {
        const createdAtMs = now();
        let configuredTimeout: unknown;
        try {
          configuredTimeout = options.timeout?.();
        } catch {
          /* Use the pinned default. */
        }
        const expiresAtMs = pendingExpiry(createdAtMs, configuredTimeout);
        if (expiresAtMs === undefined) return undefined;
        // The ID index is bounded independently of exact response ownership.
        // Unknown IDs disable hook attribution while their response handle lives.
        let unknownIds = bindings === undefined || bindings.length > MAX_BINDINGS;
        let repeatedId = false;
        const ids = new Set<string>();
        const ownedBindings: TraceBidBinding[] = [];
        const collisions = new Set<TracePendingHandle>();
        const refs: string[] = [];
        for (let index = 0; index < Math.min(64, carry.slotRefs.length); index += 1)
          refs.push(carry.slotRefs[index]!);
        const acceptedRefs = new Set(refs);
        if (!unknownIds && bindings) {
          for (const binding of bindings) {
            const id = hookKey(binding);
            if (id === undefined) {
              unknownIds = true;
              continue;
            }
            for (const owner of originalIds.get(id) ?? []) collisions.add(owner);
            if (ids.has(id)) {
              repeatedId = true;
              continue;
            }
            ids.add(id);
            ownedBindings.push(
              Object.freeze({
                bidId: binding.bidId,
                bidderRequestId: binding.bidderRequestId,
                ...(binding.slotRef !== undefined && acceptedRefs.has(binding.slotRef)
                  ? { slotRef: binding.slotRef }
                  : {}),
              })
            );
          }
        }
        const affected = new Set<PendingRecord>();
        let liveUnknown = false;
        for (const [handle, record] of records) {
          liveUnknown ||= record.unknownIds;
          if (unknownIds || collisions.has(handle)) {
            record.hookEligible = false;
            affected.add(record);
          }
        }
        const handle = Object.freeze({ key: Symbol() });
        const ownedCarry = Object.freeze({
          collector: carry.collector,
          slotRefs: Object.freeze(refs),
        });
        const timer = schedule(() => {
          remove(handle);
        }, expiresAtMs - createdAtMs);
        const record: PendingRecord = {
          bindings: Object.freeze(ownedBindings),
          carry: ownedCarry,
          expiresAtMs,
          timer,
          unknownIds,
          hookEligible:
            !!ownedBindings.length &&
            !unknownIds &&
            !liveUnknown &&
            !repeatedId &&
            !collisions.size,
        };
        records.set(handle, record);
        for (const id of ids) {
          const owners = originalIds.get(id) ?? new Set<TracePendingHandle>();
          owners.add(handle);
          originalIds.set(id, owners);
        }
        if (records.size > MAX_PENDING) remove(records.keys().next().value);
        if (!record.hookEligible) affected.add(record);
        for (const unavailable of affected) noteUnavailable(unavailable);
        return handle;
      } catch {
        return undefined;
      }
    },
    response(handle: TracePendingHandle | undefined, body: unknown) {
      observeTraceApiResponse(take(handle), body);
    },
    failure(bids: readonly Pick<TraceBidBinding, 'bidId' | 'bidderRequestId'>[]) {
      try {
        if (bids.length > MAX_BINDINGS) return;
        const handles = new Set<TracePendingHandle>();
        for (const bid of bids) {
          const id = hookKey(bid);
          if (id === undefined) continue;
          const owners = originalIds.get(id);
          if (owners?.size === 1) {
            const handle = owners.values().next().value;
            if (handle && records.get(handle)?.hookEligible) handles.add(handle);
          }
        }
        for (const handle of handles) observeTraceApiFailure(take(handle));
      } catch {
        /* No raw callback input is retained or reported. */
      }
    },
    clear,
    destroy() {
      destroyed = true;
      clear();
    },
  });
}
