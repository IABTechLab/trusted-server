import { boundedJsonShape, traceJsonSnapshot } from './json';
export { boundedJsonShape, traceJsonSnapshot } from './json';
import {
  traceInteger,
  traceKeys,
  traceObject,
  traceOwn,
  traceText,
  traceTimestamp,
  validateTraceRequestContext,
} from './context';
import {
  TRACE_ATTRIBUTION_REASONS,
  TRACE_BINDING_REASONS,
  TRACE_CALLBACK_KINDS,
  TRACE_CALLBACK_REASONS,
  TRACE_COVERAGE_ISSUES,
  TRACE_CREATIVE_FAILURES,
  TRACE_DELIVERIES,
  TRACE_OPPORTUNITIES,
  TRACE_REQUEST_PATHS,
  TRACE_RESPONSE_CLASSES,
  type TraceGptDiagnosticsV1,
  type TraceReportV1,
  type TraceStoredReportV1,
} from './report-types';
import {
  traceEnum,
  traceItems,
  traceSize,
  validDiagnosticAuctionId,
  validateTraceAuctionEvidence,
  validateTraceSlotCorrelation,
} from './validation';

export const TRACE_CYCLE_KEYS = [
  'requestNumber',
  'requestedAtMs',
  'responseAtMs',
  'renderAtMs',
  'loadAtMs',
  'viewableAtMs',
  'durations',
  'isEmpty',
  'requestedSlotSizes',
  'size',
  'observedSlotSize',
  'isBackfill',
  'slotContentChanged',
  'incompleteSequence',
  'responseClass',
  'requestPath',
  'requestIntentId',
  'trustedServerAuctionId',
  'opportunityToRequestMs',
  'replacedRequestNumber',
  'previousRenderToRequestMs',
  'creativeChanged',
  'loadObservedBeforeRender',
  'trustedServerOpportunity',
  'trustedServerCreativeRequestAtMs',
  'trustedServerCreativeResponseAtMs',
  'trustedServerCreativeFailures',
  'delivery',
] as const;
const DURATION_KEYS = [
  'requestToResponseMs',
  'responseToRenderMs',
  'requestToRenderMs',
  'renderToLoadMs',
  'renderToViewableMs',
] as const;
const TIME_KEYS = [
  'requestedAtMs',
  'responseAtMs',
  'renderAtMs',
  'loadAtMs',
  'viewableAtMs',
  'opportunityToRequestMs',
  'previousRenderToRequestMs',
  'trustedServerCreativeRequestAtMs',
  'trustedServerCreativeResponseAtMs',
] as const;
const BOOLEAN_KEYS = [
  'isEmpty',
  'isBackfill',
  'slotContentChanged',
  'creativeChanged',
  'loadObservedBeforeRender',
] as const;
const TRUNCATION_KEYS = [
  'omitted_server_auctions',
  'omitted_slot_correlations',
  'omitted_request_cycles',
  'omitted_callback_issues',
  'omitted_attribution_issues',
  'omitted_nested_values',
] as const;

/** Ingests a report into an independently owned immutable public model. */
export function parseTraceReport(
  value: unknown,
  origin: string,
  storedAtMs: number
): TraceReportV1 | undefined {
  const snapshot = traceJsonSnapshot(value);
  if (
    !snapshot ||
    traceOrigin(origin) !== origin ||
    !traceNumber(storedAtMs) ||
    !report(snapshot.value, origin, storedAtMs)
  )
    return undefined;
  freezeOwned(snapshot.value);
  return snapshot.value;
}

/** Ingests a supported, fresh storage wrapper without retaining caller objects. */
export function parseTraceStoredReport(
  value: unknown,
  origin: string,
  nowMs: number
): TraceStoredReportV1 | undefined {
  const snapshot = traceJsonSnapshot(value, 11);
  if (!snapshot || !validateTraceStoredReport(snapshot.value, origin, nowMs)) return undefined;
  freezeOwned(snapshot.value);
  return snapshot.value;
}

function freezeOwned(value: unknown): void {
  if (typeof value !== 'object' || value === null) return;
  Object.values(value).forEach(freezeOwned);
  Object.freeze(value);
}

/** Safe categories for incompatible source/public schema versions. */
export type TraceReportRejection =
  | 'unsupported_report_version'
  | 'unsupported_auction_version'
  | 'unsupported_correlation_version'
  | 'unsupported_gpt_version'
  | 'unsupported_gpt_source_version'
  | 'invalid_report';

/** Classifies invalid reports without returning input values or parser errors. */
export function traceReportRejection(
  value: unknown,
  origin: string,
  storedAtMs: number
): TraceReportRejection | undefined {
  try {
    const snapshot = traceJsonSnapshot(value);
    if (!snapshot) return 'invalid_report';
    value = snapshot.value;
    if (validateTraceReport(value, origin, storedAtMs)) return undefined;
    if (!traceObject(value)) return 'invalid_report';
    if (traceOwn(value, 'schema_version') && value.schema_version !== 1)
      return 'unsupported_report_version';
    const gpt = value.gpt_diagnostics;
    if (traceObject(gpt)) {
      if (traceOwn(gpt, 'schema_version') && gpt.schema_version !== 1)
        return 'unsupported_gpt_version';
      if (traceOwn(gpt, 'source_schema_version') && gpt.source_schema_version !== 1)
        return 'unsupported_gpt_source_version';
    }
    const auctions = traceItems(value.server_auctions, 16);
    if (
      auctions?.some(
        (item) => traceObject(item) && traceOwn(item, 'schema_version') && item.schema_version !== 1
      )
    )
      return 'unsupported_auction_version';
    const correlations = traceItems(value.slot_correlations, 128);
    if (
      correlations?.some(
        (item) => traceObject(item) && traceOwn(item, 'schema_version') && item.schema_version !== 1
      )
    )
      return 'unsupported_correlation_version';
    return 'invalid_report';
  } catch {
    return 'invalid_report';
  }
}

/** Validates a browser-relative numeric clock without truncating fractional observations. */
export function traceNumber(value: unknown, maximum = Number.MAX_SAFE_INTEGER): value is number {
  return typeof value === 'number' && Number.isFinite(value) && value >= 0 && value <= maximum;
}

/** Canonicalizes only a strict bare HTTP(S) origin, with no URL repair. */
export function traceOrigin(value: unknown): string | undefined {
  if (!traceText(value, 255) || !/^https?:\/\//i.test(value)) return undefined;
  const authority = value.slice(value.indexOf('://') + 3);
  if (!authority || /[\s/@?#,\\]/.test(authority)) return undefined;
  const port = authority.startsWith('[')
    ? authority.slice(authority.indexOf(']') + 1)
    : authority.includes(':')
      ? authority.slice(authority.lastIndexOf(':'))
      : '';
  if (port !== '' && (!/^:\d+$/.test(port) || Number(port.slice(1)) > 65535)) return undefined;
  try {
    const parsed = new URL(value);
    if (
      !['http:', 'https:'].includes(parsed.protocol) ||
      !parsed.hostname ||
      parsed.username ||
      parsed.password ||
      parsed.search ||
      parsed.hash ||
      parsed.pathname !== '/'
    )
      return undefined;
    return parsed.origin;
  } catch {
    return undefined;
  }
}

function members(value: unknown, limit: number, predicate: (item: unknown) => boolean): boolean {
  const items = traceItems(value, limit);
  return items !== undefined && items.every(predicate);
}
function optional(
  value: Record<string, unknown>,
  key: string,
  valid: (item: unknown) => boolean
): boolean {
  return !traceOwn(value, key) || valid(value[key]);
}
function cycle(value: unknown): boolean {
  if (
    !traceObject(value) ||
    !traceKeys(value, ['requestNumber', 'durations', 'incompleteSequence'], TRACE_CYCLE_KEYS) ||
    !traceInteger(value.requestNumber) ||
    typeof value.incompleteSequence !== 'boolean'
  )
    return false;
  if (
    !traceObject(value.durations) ||
    !traceKeys(value.durations, [], DURATION_KEYS) ||
    !Object.values(value.durations).every((item) => traceNumber(item))
  )
    return false;
  if (
    !TIME_KEYS.every((key) => optional(value, key, traceNumber)) ||
    !BOOLEAN_KEYS.every((key) => optional(value, key, (item) => typeof item === 'boolean'))
  )
    return false;
  return (
    ['requestIntentId', 'replacedRequestNumber'].every((key) =>
      optional(value, key, traceInteger)
    ) &&
    optional(value, 'trustedServerAuctionId', validDiagnosticAuctionId) &&
    optional(value, 'requestedSlotSizes', (item) => members(item, 16, (size) => traceSize(size))) &&
    optional(value, 'size', (item) => traceSize(item)) &&
    optional(value, 'observedSlotSize', (item) => traceSize(item, true)) &&
    optional(value, 'responseClass', (item) => traceEnum(item, TRACE_RESPONSE_CLASSES)) &&
    optional(value, 'requestPath', (item) => traceEnum(item, TRACE_REQUEST_PATHS)) &&
    optional(value, 'trustedServerOpportunity', (item) => traceEnum(item, TRACE_OPPORTUNITIES)) &&
    optional(value, 'delivery', (item) => traceEnum(item, TRACE_DELIVERIES)) &&
    optional(value, 'trustedServerCreativeFailures', (item) =>
      members(item, 16, (failure) => traceEnum(failure, TRACE_CREATIVE_FAILURES))
    )
  );
}
function slot(value: unknown): boolean {
  if (
    !traceObject(value) ||
    !traceKeys(
      value,
      ['runtimeSlotNumber', 'binding', 'requests'],
      ['currentVisibilityPercentage', 'maximumVisibilityPercentage']
    ) ||
    !traceInteger(value.runtimeSlotNumber)
  )
    return false;
  if (
    !traceObject(value.binding) ||
    !traceKeys(value.binding, ['status'], ['reason']) ||
    !traceEnum(value.binding.status, ['bound', 'unbound', 'ambiguous']) ||
    !optional(value.binding, 'reason', (item) => traceEnum(item, TRACE_BINDING_REASONS))
  )
    return false;
  return (
    optional(value, 'currentVisibilityPercentage', (item) => traceNumber(item, 100)) &&
    optional(value, 'maximumVisibilityPercentage', (item) => traceNumber(item, 100)) &&
    members(value.requests, 10, cycle)
  );
}
function callback(value: unknown): boolean {
  return (
    traceObject(value) &&
    traceKeys(value, ['kind', 'runtimeSlotNumber', 'timestampMs', 'disposition', 'reason']) &&
    traceEnum(value.kind, TRACE_CALLBACK_KINDS) &&
    traceInteger(value.runtimeSlotNumber) &&
    traceNumber(value.timestampMs) &&
    traceEnum(value.disposition, ['matched', 'unmatched', 'ambiguous']) &&
    traceEnum(value.reason, TRACE_CALLBACK_REASONS)
  );
}
function attribution(value: unknown): boolean {
  return (
    traceObject(value) &&
    traceKeys(value, ['reason', 'timestampMs'], ['runtimeSlotNumber']) &&
    traceEnum(value.reason, TRACE_ATTRIBUTION_REASONS) &&
    traceNumber(value.timestampMs) &&
    optional(value, 'runtimeSlotNumber', traceInteger)
  );
}
function coverageCounters(value: unknown): boolean {
  return (
    traceObject(value) &&
    traceKeys(value, ['observed', 'matched', 'unmatched', 'ambiguous']) &&
    Object.values(value).every((item) => traceInteger(item))
  );
}
function gpt(value: unknown, origin: string): value is TraceGptDiagnosticsV1 {
  if (
    !traceObject(value) ||
    !traceKeys(
      value,
      [
        'schema_version',
        'source_schema_version',
        'capturedAt',
        'page',
        'slots',
        'callbackIssues',
        'coverage',
        'metadata',
      ],
      ['attributionIssues']
    ) ||
    value.schema_version !== 1 ||
    value.source_schema_version !== 1 ||
    !traceTimestamp(value.capturedAt)
  )
    return false;
  if (
    !traceObject(value.page) ||
    !traceKeys(value.page, ['origin', 'pathname']) ||
    traceOrigin(value.page.origin) !== origin ||
    value.page.pathname !== '/[redacted]'
  )
    return false;
  if (
    !members(value.slots, 64, slot) ||
    !members(value.callbackIssues, 128, callback) ||
    !optional(value, 'attributionIssues', (item) => members(item, 128, attribution))
  )
    return false;
  if (
    !traceObject(value.coverage) ||
    !traceKeys(value.coverage, TRACE_CALLBACK_KINDS) ||
    !Object.values(value.coverage).every(coverageCounters)
  )
    return false;
  return (
    traceObject(value.metadata) &&
    traceKeys(
      value.metadata,
      ['droppedCallbacks', 'evictedSlots', 'evictedRequestCycles'],
      ['droppedAttributionIssues']
    ) &&
    Object.values(value.metadata).every((item) => traceInteger(item))
  );
}

/** Validates the supported public GPT projection before display or export. */
export function validateTraceGptDiagnostics(
  value: unknown,
  origin: string
): value is TraceGptDiagnosticsV1 {
  try {
    const snapshot = traceJsonSnapshot(value, 10, 4 * 1024 * 1024);
    return snapshot !== undefined && traceOrigin(origin) === origin && gpt(snapshot.value, origin);
  } catch {
    return false;
  }
}

function closeTimestamp(value: unknown, storedAtMs: number): boolean {
  if (!traceTimestamp(value)) return false;
  const time = Date.parse(value);
  return Number.isFinite(time) && Math.abs(time - storedAtMs) <= 60000;
}
function report(value: unknown, origin: string, storedAtMs: number): value is TraceReportV1 {
  if (
    !traceObject(value) ||
    !traceKeys(value, [
      'schema_version',
      'captured_at',
      'request_context',
      'server_auctions',
      'slot_correlations',
      'gpt_diagnostics',
      'auction_coverage',
      'truncation',
    ]) ||
    value.schema_version !== 1 ||
    !closeTimestamp(value.captured_at, storedAtMs)
  )
    return false;
  if (
    !validateTraceRequestContext(value.request_context) ||
    !members(value.server_auctions, 16, validateTraceAuctionEvidence) ||
    !members(value.slot_correlations, 128, validateTraceSlotCorrelation) ||
    !gpt(value.gpt_diagnostics, origin) ||
    !closeTimestamp(value.gpt_diagnostics.capturedAt, storedAtMs)
  )
    return false;
  if (
    !traceObject(value.truncation) ||
    !traceKeys(value.truncation, TRUNCATION_KEYS) ||
    !Object.values(value.truncation).every((item) => traceInteger(item, 65535))
  )
    return false;
  const coverage = value.auction_coverage;
  if (!traceObject(coverage) || !traceKeys(coverage, ['capture_status', 'issues'])) return false;
  const issues = traceItems(coverage.issues, 16);
  if (!issues || !issues.every((issue) => traceEnum(issue, TRACE_COVERAGE_ISSUES))) return false;
  let previous = -1;
  for (const issue of issues) {
    const index = TRACE_COVERAGE_ISSUES.indexOf(issue as (typeof TRACE_COVERAGE_ISSUES)[number]);
    if (index <= previous) return false;
    previous = index;
  }
  const records = traceItems(value.server_auctions, 16);
  if (!records) return false;
  const correlations = traceItems(value.slot_correlations, 128);
  if (!correlations) return false;
  for (const correlation of correlations) {
    if (!traceObject(correlation)) return false;
    if (
      records.some(
        (record) =>
          traceObject(record) &&
          record.source === 'auction_api' &&
          record.diagnostic_auction_id === correlation.diagnostic_auction_id
      )
    )
      return false;
  }
  const failed = issues.some((issue) =>
    [
      'evidence_projection_failed',
      'evidence_transport_failed',
      'evidence_validation_failed',
      'record_evicted',
    ].includes(issue as string)
  );
  const expected =
    records.length > 0
      ? failed
        ? 'partial'
        : 'complete'
      : failed
        ? 'unavailable'
        : 'not_observed';
  return coverage.capture_status === expected && boundedJsonShape(value, 10) !== undefined;
}

/** Validates the exact combined report and its capture-clock relationship. */
export function validateTraceReport(
  value: unknown,
  origin: string,
  storedAtMs: number
): value is TraceReportV1 {
  try {
    const snapshot = traceJsonSnapshot(value);
    return (
      snapshot !== undefined &&
      traceOrigin(origin) === origin &&
      traceNumber(storedAtMs) &&
      report(snapshot.value, origin, storedAtMs)
    );
  } catch {
    return false;
  }
}
/** Validates the exact storage wrapper, bounds and expiry before report use. */
export function validateTraceStoredReport(
  value: unknown,
  origin: string,
  nowMs: number
): value is TraceStoredReportV1 {
  try {
    const snapshot = traceJsonSnapshot(value, 11);
    if (!snapshot) return false;
    const owned = snapshot.value;
    return (
      traceObject(owned) &&
      traceKeys(owned, ['stored_at_ms', 'report']) &&
      traceNumber(nowMs) &&
      traceInteger(owned.stored_at_ms) &&
      owned.stored_at_ms - nowMs <= 60000 &&
      nowMs - owned.stored_at_ms <= 900000 &&
      validateTraceReport(owned.report, origin, owned.stored_at_ms)
    );
  } catch {
    return false;
  }
}
