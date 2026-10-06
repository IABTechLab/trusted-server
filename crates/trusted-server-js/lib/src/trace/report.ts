import { traceInteger, traceKeys, traceObject, validateTraceRequestContext } from './context';
import type { TraceAuctionEvidenceV1, TraceSlotCorrelationV1 } from './types';
import { addOmissions } from './omissions';
import { projectTraceGptDiagnostics } from './projection';
import {
  TRACE_COVERAGE_ISSUES,
  type TraceCoverageIssue,
  type TraceReportV1,
  type TraceStoredReportV1,
} from './report-types';
import { parseTraceStoredReport, traceJsonSnapshot, traceOrigin } from './report-validation';
import {
  traceEnum,
  traceItems,
  parseTraceAuctionTransport,
  parseTraceSlotCorrelation,
} from './validation';

/** Collector observations keep outer losses separate from inner evidence truncation. */
export interface TraceCollectorSnapshot {
  readonly serverAuctions: readonly TraceAuctionEvidenceV1[];
  readonly slotCorrelations: readonly TraceSlotCorrelationV1[];
  readonly issues: readonly TraceCoverageIssue[];
  readonly omittedServerAuctions: number;
  readonly omittedSlotCorrelations: number;
}
export interface TraceCaptureInput {
  readonly requestContext: unknown;
  readonly gptSource: unknown;
  readonly origin: string;
  readonly capturedAtMs: number;
  readonly collector: unknown;
}
export type TraceCaptureResult =
  | { readonly ok: true; readonly value: TraceStoredReportV1 }
  | {
      readonly ok: false;
      readonly reason:
        | 'invalid_snapshot'
        | 'unsupported_source_version'
        | 'omission_counter_overflow'
        | 'snapshot_too_large';
    };
type Mutable<T> = T extends readonly (infer Element)[]
  ? Mutable<Element>[]
  : T extends object
    ? { -readonly [Key in keyof T]: Mutable<T[Key]> }
    : T;
const MAXIMUM_BYTES = 512 * 1024;

/** Captures one owned report while protecting every nonempty slot's newest cycle. */
export function buildTraceReport(
  input: TraceCaptureInput,
  maximumBytes = MAXIMUM_BYTES
): TraceCaptureResult {
  let counterFailed = false;
  const add = (current: number, added: number): number => {
    try {
      return addOmissions(current, added);
    } catch {
      counterFailed = true;
      throw new Error('omission_counter_overflow');
    }
  };
  try {
    if (
      !traceObject(input) ||
      !traceKeys(input, ['requestContext', 'gptSource', 'origin', 'capturedAtMs', 'collector'])
    )
      return { ok: false, reason: 'invalid_snapshot' };
    const descriptors = Object.getOwnPropertyDescriptors(input);
    const captureClock: unknown = descriptors.capturedAtMs.value;
    const origin: unknown = descriptors.origin.value;
    const requestContext: unknown = descriptors.requestContext.value;
    const gptSource: unknown = descriptors.gptSource.value;
    const collector: unknown = descriptors.collector.value;
    if (
      !traceInteger(captureClock) ||
      typeof origin !== 'string' ||
      traceOrigin(origin) !== origin ||
      !traceInteger(maximumBytes, MAXIMUM_BYTES) ||
      maximumBytes === 0
    )
      return { ok: false, reason: 'invalid_snapshot' };
    const capturedAt = new Date(captureClock).toISOString();
    const context = traceJsonSnapshot(requestContext);
    if (!context || !validateTraceRequestContext(context.value))
      return { ok: false, reason: 'invalid_snapshot' };
    const projected = projectTraceGptDiagnostics(gptSource, origin);
    if (!projected.ok)
      return {
        ok: false,
        reason: projected.reason === 'invalid_source' ? 'invalid_snapshot' : projected.reason,
      };
    if (Math.abs(Date.parse(projected.value.capturedAt) - captureClock) > 60000)
      return { ok: false, reason: 'invalid_snapshot' };
    const collection = traceJsonSnapshot(collector, 10, 64 * 1024 * 1024)?.value;
    if (
      !traceObject(collection) ||
      !traceKeys(collection, [
        'serverAuctions',
        'slotCorrelations',
        'issues',
        'omittedServerAuctions',
        'omittedSlotCorrelations',
      ])
    )
      return { ok: false, reason: 'invalid_snapshot' };
    const auctions = traceItems(collection.serverAuctions, 65535 + 16);
    const sidecars = traceItems(collection.slotCorrelations, 65535 + 128);
    const issueItems = traceItems(collection.issues, TRACE_COVERAGE_ISSUES.length);
    if (
      !auctions ||
      !sidecars ||
      !issueItems ||
      !issueItems.every((issue) => traceEnum(issue, TRACE_COVERAGE_ISSUES))
    )
      return { ok: false, reason: 'invalid_snapshot' };
    const copiedAuctions = auctions.map(
      (record) => parseTraceAuctionTransport({ schema_version: 1, evidence: record })?.evidence
    );
    const copiedSidecars = sidecars.map(parseTraceSlotCorrelation);
    if (
      copiedAuctions.some((record) => record === undefined) ||
      copiedSidecars.some((record) => record === undefined)
    )
      return { ok: false, reason: 'invalid_snapshot' };
    const apiIds = new Set(
      copiedAuctions
        .filter((record) => record?.source === 'auction_api')
        .map((record) => record!.diagnostic_auction_id)
    );
    if (copiedSidecars.some((sidecar) => apiIds.has(sidecar!.diagnostic_auction_id)))
      return { ok: false, reason: 'invalid_snapshot' };
    const omittedAuctions = add(
      collection.omittedServerAuctions as number,
      Math.max(0, auctions.length - 16)
    );
    let omittedSidecars = add(
      collection.omittedSlotCorrelations as number,
      Math.max(0, sidecars.length - 128)
    );
    const retainedAuctions = copiedAuctions.slice(-16);
    const discardedIds = new Set(
      copiedAuctions
        .slice(0, Math.max(0, copiedAuctions.length - 16))
        .map((record) => record!.diagnostic_auction_id)
    );
    const retainedIds = new Set(retainedAuctions.map((record) => record!.diagnostic_auction_id));
    const retainedSidecars = copiedSidecars
      .slice(-128)
      .filter(
        (sidecar) =>
          !discardedIds.has(sidecar!.diagnostic_auction_id) ||
          retainedIds.has(sidecar!.diagnostic_auction_id)
      );
    omittedSidecars = add(
      omittedSidecars,
      Math.min(128, sidecars.length) - retainedSidecars.length
    );
    const draft = traceJsonSnapshot(
      {
        schema_version: 1,
        captured_at: capturedAt,
        request_context: context.value,
        server_auctions: retainedAuctions,
        slot_correlations: retainedSidecars,
        gpt_diagnostics: projected.value,
        auction_coverage: { capture_status: 'not_observed', issues: [] },
        truncation: {
          omitted_server_auctions: omittedAuctions,
          omitted_slot_correlations: omittedSidecars,
          omitted_request_cycles: 0,
          omitted_callback_issues: 0,
          omitted_attribution_issues: 0,
          omitted_nested_values: projected.omittedNestedValues,
        },
      },
      10,
      16 * 1024 * 1024
    )?.value as Mutable<TraceReportV1> | undefined;
    if (!draft) return { ok: false, reason: 'invalid_snapshot' };
    const issues = new Set(issueItems as TraceCoverageIssue[]);
    const recompute = (): void => {
      if (draft.truncation.omitted_server_auctions > 0) issues.add('record_evicted');
      if (draft.truncation.omitted_slot_correlations > 0) issues.add('correlation_unavailable');
      const ordered = TRACE_COVERAGE_ISSUES.filter((issue) => issues.has(issue));
      const failed = ordered.some((issue) =>
        [
          'evidence_projection_failed',
          'evidence_transport_failed',
          'evidence_validation_failed',
          'record_evicted',
        ].includes(issue)
      );
      draft.auction_coverage = {
        capture_status: draft.server_auctions.length
          ? failed
            ? 'partial'
            : 'complete'
          : failed
            ? 'unavailable'
            : 'not_observed',
        issues: ordered,
      };
    };
    recompute();
    const wrapper = { stored_at_ms: captureClock, report: draft };
    // Every draft member is already independently owned data. Serialize only that
    // draft while measuring; final ingestion checks the complete schema and depth.
    const encoder = new TextEncoder();
    const fits = (): boolean => encoder.encode(JSON.stringify(wrapper)).length <= maximumBytes;
    const pruneSidecars = (
      predicate: (sidecar: Mutable<TraceReportV1>['slot_correlations'][number]) => boolean
    ): void => {
      const retained = draft.slot_correlations.filter((sidecar) => !predicate(sidecar));
      draft.truncation.omitted_slot_correlations = add(
        draft.truncation.omitted_slot_correlations,
        draft.slot_correlations.length - retained.length
      );
      draft.slot_correlations = retained;
    };
    const floors = new Set(
      draft.gpt_diagnostics.slots.flatMap((slot) =>
        slot.requests.length ? [slot.requests[slot.requests.length - 1]] : []
      )
    );
    const cycles = draft.gpt_diagnostics.slots.flatMap((slot) =>
      slot.requests.filter((cycle) => !floors.has(cycle)).map((cycle) => ({ slot, cycle }))
    );
    cycles.sort((left, right) => {
      const a = left.cycle.requestedAtMs;
      const b = right.cycle.requestedAtMs;
      return (
        (a === undefined ? (b === undefined ? 0 : -1) : b === undefined ? 1 : a - b) ||
        left.slot.runtimeSlotNumber - right.slot.runtimeSlotNumber ||
        left.cycle.requestNumber - right.cycle.requestNumber
      );
    });
    for (const { slot, cycle } of cycles) {
      if (fits()) break;
      slot.requests = slot.requests.filter((retained) => retained !== cycle);
      draft.truncation.omitted_request_cycles = add(draft.truncation.omitted_request_cycles, 1);
      pruneSidecars(
        (sidecar) =>
          sidecar.runtime_slot_number === slot.runtimeSlotNumber &&
          sidecar.request_number === cycle.requestNumber
      );
      recompute();
    }
    const removeIssues = <Issue extends { timestampMs: number }>(
      source: Issue[] | undefined,
      counter: 'omitted_callback_issues' | 'omitted_attribution_issues'
    ): void => {
      if (!source) return;
      const ordered = source
        .map((issue, index) => ({ issue, index }))
        .sort((a, b) => a.issue.timestampMs - b.issue.timestampMs || a.index - b.index);
      for (const { issue } of ordered) {
        if (fits()) break;
        const index = source.indexOf(issue);
        source.splice(index, 1);
        draft.truncation[counter] = add(draft.truncation[counter], 1);
      }
    };
    removeIssues(draft.gpt_diagnostics.callbackIssues, 'omitted_callback_issues');
    removeIssues(draft.gpt_diagnostics.attributionIssues, 'omitted_attribution_issues');
    const remainingCycles = () => draft.gpt_diagnostics.slots.flatMap((slot) => slot.requests);
    const removeAuction = (record: Mutable<TraceReportV1>['server_auctions'][number]): void => {
      const id = record.diagnostic_auction_id;
      draft.server_auctions = draft.server_auctions.filter((retained) => retained !== record);
      draft.truncation.omitted_server_auctions = add(draft.truncation.omitted_server_auctions, 1);
      for (const slot of draft.gpt_diagnostics.slots) {
        const retained = slot.requests.filter((cycle) => cycle.trustedServerAuctionId !== id);
        draft.truncation.omitted_request_cycles = add(
          draft.truncation.omitted_request_cycles,
          slot.requests.length - retained.length
        );
        slot.requests = retained;
      }
      pruneSidecars((sidecar) => sidecar.diagnostic_auction_id === id);
      recompute();
    };
    for (const record of [...draft.server_auctions]) {
      if (fits()) break;
      if (
        !remainingCycles().some(
          (cycle) => cycle.trustedServerAuctionId === record.diagnostic_auction_id
        )
      )
        removeAuction(record);
    }
    for (const record of [...draft.server_auctions]) {
      if (fits()) break;
      if (
        ![...floors].some((cycle) => cycle.trustedServerAuctionId === record.diagnostic_auction_id)
      )
        removeAuction(record);
    }
    if (!fits()) return { ok: false, reason: 'snapshot_too_large' };
    const result = parseTraceStoredReport(wrapper, origin, captureClock);
    return result ? { ok: true, value: result } : { ok: false, reason: 'invalid_snapshot' };
  } catch {
    return { ok: false, reason: counterFailed ? 'omission_counter_overflow' : 'invalid_snapshot' };
  }
}
