import type { AdRequest } from '../core/auction';
import type { TsjsApi } from '../core/types';

import { createTraceCollector, type TraceCollector } from './collector';
import { createTraceGptBridge, type TraceGptBridge } from './gpt';
import { parseTraceAuctionTransport, validTraceSlotRef } from './validation';

export interface TraceRuntimeScope {
  tsjs?: TsjsApi;
  __tsjs_trace_active?: unknown;
  crypto?: { randomUUID?(): string };
}
export interface TraceApiRequestCarry {
  readonly request: AdRequest;
  readonly slotRefs: readonly string[];
  readonly collector: TraceCollector;
}
function currentScope(): TraceRuntimeScope | undefined {
  return typeof window === 'object' ? window : undefined;
}

/** Installs one collector on the shared facade only for the literal document gate. */
export function installTraceRuntime(
  api: TsjsApi,
  scope = currentScope()
): TraceCollector | undefined {
  try {
    if (scope?.__tsjs_trace_active !== true) return undefined;
    const collector = (api.traceEvidence ??= createTraceCollector());
    api.traceGpt ??= createTraceGptBridge(collector);
    return collector;
  } catch {
    return undefined;
  }
}

/** Reads the existing activated facade without creating a collector or listeners. */
export function getActiveTraceCollector(scope = currentScope()): TraceCollector | undefined {
  try {
    return scope?.__tsjs_trace_active === true ? scope.tsjs?.traceEvidence : undefined;
  } catch {
    return undefined;
  }
}

/** Reads the single core-owned slot bridge only behind the literal document gate. */
export function getActiveTraceGptBridge(scope = currentScope()): TraceGptBridge | undefined {
  try {
    return scope?.__tsjs_trace_active === true ? scope.tsjs?.traceGpt : undefined;
  } catch {
    return undefined;
  }
}

/** Decorates only final grouped units, retaining the ordinary request on token failure. */
export function prepareTraceAuctionRequest(
  request: AdRequest,
  scope = currentScope()
): TraceApiRequestCarry | undefined {
  const collector = getActiveTraceCollector(scope);
  if (!collector) return undefined;
  const fallback = {
    request,
    slotRefs: Object.freeze([] as string[]),
    collector,
  };
  try {
    if (typeof scope?.crypto?.randomUUID !== 'function') return fallback;
    const refs = request.adUnits.map(() => `ts-slot-${scope.crypto!.randomUUID!()}`);
    if (!refs.every(validTraceSlotRef) || new Set(refs).size !== refs.length) return fallback;
    const adUnits = request.adUnits.map((unit, index) => {
      const prior = unit.ext?.trusted_server;
      return {
        ...unit,
        ext: {
          ...unit.ext,
          trusted_server: {
            ...(typeof prior === 'object' && prior !== null && !Array.isArray(prior) ? prior : {}),
            trace_slot_ref: refs[index],
          },
        },
      };
    });
    return {
      request: { ...request, adUnits },
      slotRefs: Object.freeze(refs),
      collector,
    };
  } catch {
    return fallback;
  }
}

function ownMember(
  value: unknown,
  key: string
): { present: boolean; valid: boolean; value?: unknown } {
  if (value === null || typeof value !== 'object') return { present: false, valid: true };
  try {
    const member = Object.getOwnPropertyDescriptor(value, key);
    return member
      ? {
          present: true,
          valid: Object.prototype.hasOwnProperty.call(member, 'value'),
          value: member.value,
        }
      : { present: false, valid: true };
  } catch {
    return { present: true, valid: false };
  }
}

/** Consumes optional API evidence before bids, without inspecting unrelated extensions. */
export function observeTraceApiResponse(
  carry: Pick<TraceApiRequestCarry, 'slotRefs' | 'collector'> | undefined,
  response: unknown
): void {
  if (!carry) return;
  try {
    const ext = ownMember(response, 'ext');
    if (!ext.present) return;
    if (!ext.valid) {
      carry.collector.recordTransport(null);
      return;
    }
    const trusted = ownMember(ext.value, 'trusted_server');
    if (!trusted.present) return;
    if (!trusted.valid) {
      carry.collector.recordTransport(null);
      return;
    }
    const member = ownMember(trusted.value, 'trace_auction');
    if (!member.present) return;
    const transport = member.valid ? parseTraceAuctionTransport(member.value) : undefined;
    if (!transport || (transport.evidence && transport.evidence.source !== 'auction_api')) {
      carry.collector.recordTransport(null);
      return;
    }
    if (transport.evidence && carry.slotRefs.length) {
      const expected = new Set(carry.slotRefs);
      const returned = transport.evidence.slots.map((slot) => slot.slot_ref);
      if (returned.some((ref) => !expected.has(ref)) || new Set(returned).size !== returned.length)
        carry.collector.recordInterpretationIssue('correlation_unavailable');
    }
    carry.collector.recordTransport(transport);
  } catch {
    /* Diagnostic callbacks never change ordinary bid parsing. */
  }
}

/** Records only a bounded API transport failure, without errors or response bodies. */
export function observeTraceApiFailure(
  carry: Pick<TraceApiRequestCarry, 'slotRefs' | 'collector'> | undefined
): void {
  try {
    carry?.collector.recordTransportFailure();
  } catch {
    /* Diagnostic failures never change ordinary bidding. */
  }
}
