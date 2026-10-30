import { addOmissions, omissionOverflow, isOmissionOverflow } from './omissions';
export { addOmissions } from './omissions';
import { traceInteger, traceKeys, traceObject } from './context';
import {
  TRACE_CALLBACK_KINDS,
  TRACE_CREATIVE_FAILURES,
  type TraceGptDiagnosticsV1,
} from './report-types';
import { TRACE_CYCLE_KEYS, traceOrigin, validateTraceGptDiagnostics } from './report-validation';
import { traceEnum, traceItems, traceSize } from './validation';

export type TraceProjectionResult =
  | {
      readonly ok: true;
      readonly value: TraceGptDiagnosticsV1;
      readonly omittedNestedValues: number;
    }
  | {
      readonly ok: false;
      readonly reason:
        | 'invalid_source'
        | 'unsupported_source_version'
        | 'omission_counter_overflow';
    };

function sourceObject(
  value: unknown,
  required: readonly string[],
  optional: readonly string[] = []
): Record<string, unknown> {
  if (!traceObject(value) || !traceKeys(value, required, optional))
    throw new Error('invalid_source');
  return value;
}
function data(source: Record<string, unknown>, key: string): unknown {
  return Object.getOwnPropertyDescriptor(source, key)?.value;
}
function copyOptional(
  source: Record<string, unknown>,
  output: Record<string, unknown>,
  key: string
): void {
  // The public API includes own optional undefined values while cycles are pending.
  // Omit only those values, matching their absence in its JSON export.
  const value = data(source, key);
  if (value !== undefined) output[key] = value;
}
function sourceItems(value: unknown, limit: number): unknown[] {
  const items = traceItems(value, limit);
  if (!items) throw new Error('invalid_source');
  return items;
}
function nestedItems(value: unknown): unknown[] {
  if (!Array.isArray(value)) throw new Error('invalid_source');
  const length = Object.getOwnPropertyDescriptor(value, 'length')?.value;
  if (!traceInteger(length)) throw new Error('invalid_source');
  if (length > 65535 + 16) omissionOverflow();
  return sourceItems(value, 65535 + 16);
}
function copySize(value: unknown, zero = false): readonly [number, number] {
  const items = sourceItems(value, 2);
  if (!traceSize(items, zero)) throw new Error('invalid_source');
  return [items[0], items[1]];
}
function durationProjection(value: unknown): Record<string, unknown> {
  const source = sourceObject(
    value,
    [],
    [
      'requestToResponseMs',
      'responseToRenderMs',
      'requestToRenderMs',
      'renderToLoadMs',
      'renderToViewableMs',
    ]
  );
  const output: Record<string, unknown> = {};
  copyOptional(source, output, 'requestToResponseMs');
  copyOptional(source, output, 'responseToRenderMs');
  copyOptional(source, output, 'requestToRenderMs');
  copyOptional(source, output, 'renderToLoadMs');
  copyOptional(source, output, 'renderToViewableMs');
  return output;
}
function cycleProjection(value: unknown, omissions: { value: number }): Record<string, unknown> {
  const source = sourceObject(
    value,
    ['requestNumber', 'durations', 'incompleteSequence'],
    [...TRACE_CYCLE_KEYS, 'adManager', 'previousCreativeId']
  );
  const output: Record<string, unknown> = {
    requestNumber: data(source, 'requestNumber'),
    durations: durationProjection(data(source, 'durations')),
    incompleteSequence: data(source, 'incompleteSequence'),
  };
  copyOptional(source, output, 'requestedAtMs');
  copyOptional(source, output, 'responseAtMs');
  copyOptional(source, output, 'renderAtMs');
  copyOptional(source, output, 'loadAtMs');
  copyOptional(source, output, 'viewableAtMs');
  copyOptional(source, output, 'isEmpty');
  copyOptional(source, output, 'isBackfill');
  copyOptional(source, output, 'slotContentChanged');
  copyOptional(source, output, 'responseClass');
  copyOptional(source, output, 'requestPath');
  copyOptional(source, output, 'requestIntentId');
  copyOptional(source, output, 'trustedServerAuctionId');
  copyOptional(source, output, 'opportunityToRequestMs');
  copyOptional(source, output, 'replacedRequestNumber');
  copyOptional(source, output, 'previousRenderToRequestMs');
  copyOptional(source, output, 'creativeChanged');
  copyOptional(source, output, 'loadObservedBeforeRender');
  copyOptional(source, output, 'trustedServerOpportunity');
  copyOptional(source, output, 'trustedServerCreativeRequestAtMs');
  copyOptional(source, output, 'trustedServerCreativeResponseAtMs');
  copyOptional(source, output, 'delivery');
  if (data(source, 'size') !== undefined) output.size = copySize(data(source, 'size'));
  if (data(source, 'observedSlotSize') !== undefined)
    output.observedSlotSize = copySize(data(source, 'observedSlotSize'), true);
  if (data(source, 'requestedSlotSizes') !== undefined) {
    const values = nestedItems(data(source, 'requestedSlotSizes'));
    if (!values.every((item) => traceSize(item))) throw new Error('invalid_source');
    output.requestedSlotSizes = values.slice(0, 16).map((item) => copySize(item));
    omissions.value = addOmissions(omissions.value, Math.max(0, values.length - 16));
  }
  if (data(source, 'trustedServerCreativeFailures') !== undefined) {
    const values = nestedItems(data(source, 'trustedServerCreativeFailures'));
    if (!values.every((item) => traceEnum(item, TRACE_CREATIVE_FAILURES)))
      throw new Error('invalid_source');
    output.trustedServerCreativeFailures = values.slice(0, 16);
    omissions.value = addOmissions(omissions.value, Math.max(0, values.length - 16));
  }
  return output;
}
function slotProjection(value: unknown, omissions: { value: number }): Record<string, unknown> {
  const source = sourceObject(
    value,
    ['runtimeSlotNumber', 'binding', 'requests'],
    ['slotElementId', 'adUnitPath', 'currentVisibilityPercentage', 'maximumVisibilityPercentage']
  );
  const binding = sourceObject(data(source, 'binding'), ['status'], ['reason']);
  const ownedBinding: Record<string, unknown> = { status: data(binding, 'status') };
  copyOptional(binding, ownedBinding, 'reason');
  const output: Record<string, unknown> = {
    runtimeSlotNumber: data(source, 'runtimeSlotNumber'),
    binding: ownedBinding,
    requests: sourceItems(data(source, 'requests'), 10).map((item) =>
      cycleProjection(item, omissions)
    ),
  };
  copyOptional(source, output, 'currentVisibilityPercentage');
  copyOptional(source, output, 'maximumVisibilityPercentage');
  return output;
}
function callbackProjection(value: unknown): Record<string, unknown> {
  const source = sourceObject(
    value,
    ['kind', 'runtimeSlotNumber', 'timestampMs', 'disposition', 'reason'],
    ['slotElementId']
  );
  return {
    kind: data(source, 'kind'),
    runtimeSlotNumber: data(source, 'runtimeSlotNumber'),
    timestampMs: data(source, 'timestampMs'),
    disposition: data(source, 'disposition'),
    reason: data(source, 'reason'),
  };
}
function attributionProjection(value: unknown): Record<string, unknown> {
  const source = sourceObject(
    value,
    ['reason', 'timestampMs'],
    ['runtimeSlotNumber', 'slotElementId']
  );
  const output: Record<string, unknown> = {
    reason: data(source, 'reason'),
    timestampMs: data(source, 'timestampMs'),
  };
  copyOptional(source, output, 'runtimeSlotNumber');
  return output;
}
function coverageProjection(value: unknown): Record<string, unknown> {
  const source = sourceObject(value, TRACE_CALLBACK_KINDS);
  const output: Record<string, unknown> = {};
  for (const kind of TRACE_CALLBACK_KINDS) {
    const counters = sourceObject(data(source, kind), [
      'observed',
      'matched',
      'unmatched',
      'ambiguous',
    ]);
    output[kind] = {
      observed: data(counters, 'observed'),
      matched: data(counters, 'matched'),
      unmatched: data(counters, 'unmatched'),
      ambiguous: data(counters, 'ambiguous'),
    };
  }
  return output;
}
function metadataProjection(value: unknown): Record<string, unknown> {
  const source = sourceObject(
    value,
    ['droppedCallbacks', 'evictedSlots', 'evictedRequestCycles'],
    ['droppedAttributionIssues']
  );
  const output: Record<string, unknown> = {
    droppedCallbacks: data(source, 'droppedCallbacks'),
    evictedSlots: data(source, 'evictedSlots'),
    evictedRequestCycles: data(source, 'evictedRequestCycles'),
  };
  copyOptional(source, output, 'droppedAttributionIssues');
  return output;
}
function freezeProjection(value: unknown): void {
  if (typeof value !== 'object' || value === null) return;
  Object.values(value).forEach(freezeProjection);
  Object.freeze(value);
}
/** Copies only explicitly allowlisted TS Console facts into the public GPT model. */
export function projectTraceGptDiagnostics(value: unknown, origin: string): TraceProjectionResult {
  try {
    const source = sourceObject(
      value,
      ['version', 'capturedAt', 'page', 'slots', 'callbackIssues', 'coverage', 'metadata'],
      ['attributionIssues']
    );
    if (data(source, 'version') !== 1) return { ok: false, reason: 'unsupported_source_version' };
    const page = sourceObject(data(source, 'page'), ['origin', 'pathname']);
    if (typeof data(page, 'pathname') !== 'string') throw new Error('invalid_source');
    const canonicalOrigin = traceOrigin(data(page, 'origin'));
    if (canonicalOrigin !== origin || traceOrigin(origin) !== origin)
      throw new Error('invalid_source');
    const omissions = { value: 0 };
    const projection: Record<string, unknown> = {
      schema_version: 1,
      source_schema_version: 1,
      capturedAt: data(source, 'capturedAt'),
      page: { origin: canonicalOrigin, pathname: '/[redacted]' },
      slots: sourceItems(data(source, 'slots'), 64).map((item) => slotProjection(item, omissions)),
      callbackIssues: sourceItems(data(source, 'callbackIssues'), 128).map(callbackProjection),
      coverage: coverageProjection(data(source, 'coverage')),
      metadata: metadataProjection(data(source, 'metadata')),
    };
    if (data(source, 'attributionIssues') !== undefined)
      projection.attributionIssues = sourceItems(data(source, 'attributionIssues'), 128).map(
        attributionProjection
      );
    if (!validateTraceGptDiagnostics(projection, origin)) throw new Error('invalid_source');
    freezeProjection(projection);
    return { ok: true, value: projection, omittedNestedValues: omissions.value };
  } catch (error) {
    return {
      ok: false,
      reason: isOmissionOverflow(error) ? 'omission_counter_overflow' : 'invalid_source',
    };
  }
}
