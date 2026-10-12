import { traceItems } from './shape';
import { traceJsonSnapshot } from './json';
export { traceArray, traceItems } from './shape';
import { traceInteger, traceKeys, traceObject, traceOwn } from './context';
import {
  TRACE_AUCTION_SOURCES,
  TRACE_TERMINAL_STATUSES,
  TRACE_TERMINAL_REASONS,
  TRACE_PROVIDER_ROLES,
  TRACE_PROVIDER_STATUSES,
  TRACE_CANDIDATES,
  type TraceAuctionEvidenceV1,
  type TraceAuctionTransportV1,
  type TraceSlotCorrelationV1,
  type TraceProviderCall,
  type TraceAuctionSlot,
} from './types';

const AUCTION_TOKEN = /^ts-auc-[0-9a-f]{12}4[0-9a-f]{3}[89ab][0-9a-f]{15}$/;
const SLOT_TOKEN = /^ts-slot-[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/;

/** Checks the exact public auction token without normalization. */
export function validDiagnosticAuctionId(value: unknown): value is string {
  return typeof value === 'string' && value.length === 39 && AUCTION_TOKEN.test(value);
}

/** Checks the exact opaque slot-reference token without normalization. */
export function validTraceSlotRef(value: unknown): value is string {
  return typeof value === 'string' && value.length === 44 && SLOT_TOKEN.test(value);
}

/** Requires an exact enum member without coercion. */
export function traceEnum<T extends string>(value: unknown, members: readonly T[]): value is T {
  return typeof value === 'string' && members.includes(value as T);
}

/** Validates a requested/creative dimension, or an explicitly zero-capable CSS box. */
export function traceSize(value: unknown, allowZero = false): value is [number, number] {
  const items = traceItems(value, 2);
  return (
    items !== undefined &&
    items.length === 2 &&
    items.every((dimension) => traceInteger(dimension, 100_000) && (allowZero || dimension > 0))
  );
}

function optionalInteger(value: Record<string, unknown>, name: string, maximum: number): boolean {
  return !traceOwn(value, name) || traceInteger(value[name], maximum);
}

function provider(value: unknown): boolean {
  return (
    traceObject(value) &&
    traceKeys(
      value,
      ['provider_number', 'role', 'status', 'returned_bid_count'],
      ['response_time_ms']
    ) &&
    traceInteger(value.provider_number, 65_535) &&
    value.provider_number > 0 &&
    traceEnum(value.role, TRACE_PROVIDER_ROLES) &&
    traceEnum(value.status, TRACE_PROVIDER_STATUSES) &&
    traceInteger(value.returned_bid_count, 65_535) &&
    optionalInteger(value, 'response_time_ms', 0xffff_ffff)
  );
}

function slot(value: unknown): boolean {
  return (
    traceObject(value) &&
    traceKeys(
      value,
      ['slot_number', 'slot_ref', 'requested_sizes', 'returned_bid_count', 'candidate'],
      ['selected_creative_size']
    ) &&
    traceInteger(value.slot_number, 65_535) &&
    value.slot_number > 0 &&
    validTraceSlotRef(value.slot_ref) &&
    traceInteger(value.returned_bid_count, 65_535) &&
    traceEnum(value.candidate, TRACE_CANDIDATES) &&
    arrayMembers(value.requested_sizes, 16, (size) => traceSize(size)) &&
    (!traceOwn(value, 'selected_creative_size') || traceSize(value.selected_creative_size))
  );
}

function evidence(value: unknown): value is TraceAuctionEvidenceV1 {
  return (
    traceObject(value) &&
    traceKeys(
      value,
      [
        'schema_version',
        'diagnostic_auction_id',
        'source',
        'terminal_status',
        'provider_calls',
        'slots',
        'truncation',
        'coverage',
      ],
      ['terminal_reason', 'total_time_ms']
    ) &&
    value.schema_version === 1 &&
    validDiagnosticAuctionId(value.diagnostic_auction_id) &&
    traceEnum(value.source, TRACE_AUCTION_SOURCES) &&
    traceEnum(value.terminal_status, TRACE_TERMINAL_STATUSES) &&
    (!traceOwn(value, 'terminal_reason') ||
      traceEnum(value.terminal_reason, TRACE_TERMINAL_REASONS)) &&
    optionalInteger(value, 'total_time_ms', 0xffff_ffff) &&
    arrayMembers(value.provider_calls, 16, provider) &&
    arrayMembers(value.slots, 64, slot) &&
    traceObject(value.truncation) &&
    traceKeys(value.truncation, [
      'omitted_provider_calls',
      'omitted_slots',
      'omitted_nested_values',
    ]) &&
    Object.values(value.truncation).every((count) => traceInteger(count, 65_535)) &&
    traceObject(value.coverage) &&
    traceKeys(value.coverage, ['provider_to_slot_no_bid']) &&
    value.coverage.provider_to_slot_no_bid === 'unavailable'
  );
}

function arrayMembers(value: unknown, limit: number, valid: (item: unknown) => boolean): boolean {
  const items = traceItems(value, limit);
  return items !== undefined && items.every(valid);
}

/** Validates the exact server-produced evidence model without modifying it. */
export function validateTraceAuctionEvidence(value: unknown): value is TraceAuctionEvidenceV1 {
  try {
    const snapshot = traceJsonSnapshot(value);
    return snapshot !== undefined && evidence(snapshot.value);
  } catch {
    return false;
  }
}

/** Validates the exclusive evidence-or-unavailable transport envelope. */
function transport(value: unknown): value is TraceAuctionTransportV1 {
  try {
    if (!traceObject(value) || value.schema_version !== 1) return false;
    if (traceOwn(value, 'evidence'))
      return traceKeys(value, ['schema_version', 'evidence']) && evidence(value.evidence);
    return (
      traceKeys(value, ['schema_version', 'unavailable_reason']) &&
      value.unavailable_reason === 'evidence_projection_failed'
    );
  } catch {
    return false;
  }
}

/** Validates a descriptor-owned observation of the exact transport envelope. */
export function validateTraceAuctionTransport(value: unknown): value is TraceAuctionTransportV1 {
  const snapshot = traceJsonSnapshot(value);
  return snapshot !== undefined && transport(snapshot.value);
}

/** Validates the trace-only exact-token GPT correlation sidecar. */
function correlation(value: unknown): value is TraceSlotCorrelationV1 {
  try {
    return (
      traceObject(value) &&
      traceKeys(value, [
        'schema_version',
        'diagnostic_auction_id',
        'slot_ref',
        'runtime_slot_number',
        'request_number',
      ]) &&
      value.schema_version === 1 &&
      validDiagnosticAuctionId(value.diagnostic_auction_id) &&
      validTraceSlotRef(value.slot_ref) &&
      traceInteger(value.runtime_slot_number) &&
      value.runtime_slot_number > 0 &&
      traceInteger(value.request_number) &&
      value.request_number > 0
    );
  } catch {
    return false;
  }
}

/** Validates one descriptor-owned observation of an exact GPT sidecar. */
export function validateTraceSlotCorrelation(value: unknown): value is TraceSlotCorrelationV1 {
  const snapshot = traceJsonSnapshot(value);
  return snapshot !== undefined && correlation(snapshot.value);
}

/** Copies a validated envelope into an immutable public model. */
export function parseTraceAuctionTransport(value: unknown): TraceAuctionTransportV1 | undefined {
  try {
    const snapshot = traceJsonSnapshot(value);
    if (!snapshot) return undefined;
    value = snapshot.value;
    if (!transport(value)) return undefined;
    if (value.evidence === undefined)
      return Object.freeze({
        schema_version: 1,
        unavailable_reason: 'evidence_projection_failed',
      });
    const original = value.evidence;
    const copied: TraceAuctionTransportV1 = Object.freeze({
      schema_version: 1,
      evidence: Object.freeze({
        schema_version: 1,
        diagnostic_auction_id: original.diagnostic_auction_id,
        source: original.source,
        terminal_status: original.terminal_status,
        ...(traceOwn(original, 'terminal_reason')
          ? { terminal_reason: original.terminal_reason }
          : {}),
        ...(traceOwn(original, 'total_time_ms') ? { total_time_ms: original.total_time_ms } : {}),
        provider_calls: Object.freeze(copyItems(original.provider_calls, 16, copyProvider)),
        slots: Object.freeze(copyItems(original.slots, 64, copySlot)),
        truncation: Object.freeze({
          omitted_provider_calls: original.truncation.omitted_provider_calls,
          omitted_slots: original.truncation.omitted_slots,
          omitted_nested_values: original.truncation.omitted_nested_values,
        }),
        coverage: Object.freeze({
          provider_to_slot_no_bid: original.coverage.provider_to_slot_no_bid,
        }),
      }),
    });
    return transport(copied) ? copied : undefined;
  } catch {
    return undefined;
  }
}

function copySize(value: readonly [number, number]): readonly [number, number] {
  const items = traceItems(value, 2);
  if (!items || items.length !== 2) throw new Error('Invalid trace size');
  return Object.freeze([items[0] as number, items[1] as number]);
}

function copyItems<T, U>(value: readonly T[], limit: number, copy: (item: T) => U): U[] {
  const items = traceItems(value, limit);
  if (!items) throw new Error('Invalid trace array');
  const owned: U[] = [];
  for (let index = 0; index < items.length; index += 1) owned.push(copy(items[index] as T));
  return owned;
}

function copyProvider(value: TraceProviderCall): TraceProviderCall {
  return Object.freeze({
    provider_number: value.provider_number,
    role: value.role,
    status: value.status,
    returned_bid_count: value.returned_bid_count,
    ...(traceOwn(value, 'response_time_ms') ? { response_time_ms: value.response_time_ms } : {}),
  });
}

function copySlot(value: TraceAuctionSlot): TraceAuctionSlot {
  return Object.freeze({
    slot_number: value.slot_number,
    slot_ref: value.slot_ref,
    requested_sizes: Object.freeze(copyItems(value.requested_sizes, 16, copySize)),
    returned_bid_count: value.returned_bid_count,
    candidate: value.candidate,
    ...(traceOwn(value, 'selected_creative_size')
      ? { selected_creative_size: copySize(value.selected_creative_size!) }
      : {}),
  });
}

/** Copies a validated correlation decision into an immutable public model. */
export function parseTraceSlotCorrelation(value: unknown): TraceSlotCorrelationV1 | undefined {
  try {
    const snapshot = traceJsonSnapshot(value);
    if (!snapshot) return undefined;
    value = snapshot.value;
    if (!correlation(value)) return undefined;
    const copied: TraceSlotCorrelationV1 = Object.freeze({
      schema_version: 1,
      diagnostic_auction_id: value.diagnostic_auction_id,
      slot_ref: value.slot_ref,
      runtime_slot_number: value.runtime_slot_number,
      request_number: value.request_number,
    });
    return correlation(copied) ? copied : undefined;
  } catch {
    return undefined;
  }
}
