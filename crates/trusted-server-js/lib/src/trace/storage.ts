import { traceObject } from './context';
import type { TraceStoredReportV1 } from './report-types';
import {
  parseTraceStoredReport,
  traceJsonSnapshot,
  traceReportRejection,
  type TraceReportRejection,
} from './report-validation';

/** One explicit snapshot replaces the prior report in this browsing context. */
export const TRACE_REPORT_STORAGE_KEY = 'trusted-server.trace.report.v1';
type TraceStorage = Pick<Storage, 'getItem' | 'setItem' | 'removeItem'>;
export type TraceStorageReadResult =
  | { readonly status: 'ready'; readonly value: TraceStoredReportV1 }
  | { readonly status: 'absent' | 'unavailable' }
  | { readonly status: 'rejected'; readonly reason: TraceReportRejection };
export type TraceStorageWriteResult =
  | { readonly status: 'stored' | 'unavailable' }
  | { readonly status: 'rejected'; readonly reason: TraceReportRejection };

function storageOrDefault(storage?: TraceStorage): TraceStorage {
  return storage ?? window.sessionStorage;
}
function rejected(value: unknown, origin: string, nowMs: number): TraceReportRejection {
  const owned = traceJsonSnapshot(value, 11)?.value;
  if (!traceObject(owned)) return 'invalid_report';
  return traceReportRejection(owned.report, origin, nowMs) ?? 'invalid_report';
}

/** Reads and validates once; rejected entries are ignored even if deletion fails. */
export function readTraceReport(
  origin: string,
  nowMs: number,
  storage?: TraceStorage
): TraceStorageReadResult {
  let available: TraceStorage;
  let serialized: string | null;
  try {
    available = storageOrDefault(storage);
    serialized = available.getItem(TRACE_REPORT_STORAGE_KEY);
  } catch {
    return { status: 'unavailable' };
  }
  if (serialized === null) return { status: 'absent' };
  let value: unknown;
  let result: TraceStoredReportV1 | undefined;
  try {
    // Bound serialized UTF-8 before parsing, including otherwise ignorable whitespace.
    if (
      typeof serialized === 'string' &&
      new TextEncoder().encode(serialized).length <= 512 * 1024
    ) {
      value = JSON.parse(serialized) as unknown;
      result = parseTraceStoredReport(value, origin, nowMs);
    }
  } catch {
    /* A bounded rejection is returned below without parser text. */
  }
  if (result) return { status: 'ready', value: result };
  const reason = rejected(value, origin, nowMs);
  try {
    available.removeItem(TRACE_REPORT_STORAGE_KEY);
  } catch {
    /* Rejected data remains ignored. */
  }
  return { status: 'rejected', reason };
}

/** Stores only a fresh validated wrapper after an explicit capture action. */
export function storeTraceReport(
  value: unknown,
  origin: string,
  nowMs: number,
  storage?: TraceStorage
): TraceStorageWriteResult {
  const owned = parseTraceStoredReport(value, origin, nowMs);
  if (!owned) return { status: 'rejected', reason: rejected(value, origin, nowMs) };
  try {
    storageOrDefault(storage).setItem(TRACE_REPORT_STORAGE_KEY, JSON.stringify(owned));
    return { status: 'stored' };
  } catch {
    return { status: 'unavailable' };
  }
}

/** Deletes only the trace-owned key on an explicit cleanup action. */
export function deleteTraceReport(storage?: TraceStorage): {
  readonly status: 'deleted' | 'unavailable';
} {
  try {
    storageOrDefault(storage).removeItem(TRACE_REPORT_STORAGE_KEY);
    return { status: 'deleted' };
  } catch {
    return { status: 'unavailable' };
  }
}
