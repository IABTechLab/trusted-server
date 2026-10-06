import { addOmissions } from './omissions';
import type { TraceCollectorSnapshot } from './report';
import { TRACE_COVERAGE_ISSUES, type TraceCoverageIssue } from './report-types';
import type { TraceAuctionEvidenceV1, TraceSlotCorrelationV1 } from './types';
import { parseTraceAuctionTransport, parseTraceSlotCorrelation, traceEnum } from './validation';

export type TraceCollectorRead =
  | { readonly ok: true; readonly value: TraceCollectorSnapshot; readonly reason?: never }
  | { readonly ok: false; readonly reason: 'omission_counter_overflow'; readonly value?: never };
/** One shared page-local collector; caller integration supplies the strict activation gate. */
export interface TraceCollector {
  recordTransport(value: unknown): void;
  recordTransportFailure(): void;
  recordCorrelation(value: unknown): void;
  recordInterpretationIssue(
    issue: 'correlation_unavailable' | 'external_client_side_unobservable'
  ): void;
  snapshot(): TraceCollectorRead;
  captureStatus(): 'complete' | 'partial' | 'unavailable' | 'not_observed';
  destroy(): void;
}

/** Retains bounded owned observations in memory without storage or network effects. */
export function createTraceCollector(): TraceCollector {
  let records: TraceAuctionEvidenceV1[] = [];
  let sidecars: TraceSlotCorrelationV1[] = [];
  const issues = new Set<TraceCoverageIssue>();
  let omittedAuctions = 0;
  let omittedSidecars = 0;
  let overflow = false;
  let destroyed = false;
  const active = () => !destroyed && !overflow;
  const add = (current: number, added: number): number => {
    try {
      return addOmissions(current, added);
    } catch {
      overflow = true;
      return current;
    }
  };
  const prune = (id: string): void => {
    const retained = sidecars.filter((sidecar) => sidecar.diagnostic_auction_id !== id);
    const removed = sidecars.length - retained.length;
    if (!removed) return;
    omittedSidecars = add(omittedSidecars, removed);
    sidecars = retained;
    issues.add('correlation_unavailable');
  };
  const api: TraceCollector = {
    recordTransport(value) {
      if (!active() || value === undefined) return;
      const transport = parseTraceAuctionTransport(value);
      if (!transport) {
        issues.add('evidence_validation_failed');
        return;
      }
      if (!transport.evidence) {
        issues.add('evidence_projection_failed');
        return;
      }
      const record = transport.evidence;
      if (record.source === 'auction_api') {
        issues.add('correlation_unavailable');
        prune(record.diagnostic_auction_id);
      }
      records.push(record);
      if (records.length <= 16) return;
      const evicted = records.shift()!;
      omittedAuctions = add(omittedAuctions, 1);
      issues.add('record_evicted');
      if (
        !records.some(
          (retained) => retained.diagnostic_auction_id === evicted.diagnostic_auction_id
        )
      )
        prune(evicted.diagnostic_auction_id);
    },
    recordTransportFailure() {
      if (active()) issues.add('evidence_transport_failed');
    },
    recordCorrelation(value) {
      if (!active()) return;
      const sidecar = parseTraceSlotCorrelation(value);
      if (
        !sidecar ||
        records.some(
          (record) =>
            record.source === 'auction_api' &&
            record.diagnostic_auction_id === sidecar.diagnostic_auction_id
        )
      ) {
        issues.add('correlation_unavailable');
        return;
      }
      sidecars.push(sidecar);
      if (sidecars.length <= 128) return;
      sidecars.shift();
      omittedSidecars = add(omittedSidecars, 1);
      issues.add('correlation_unavailable');
    },
    recordInterpretationIssue(issue) {
      if (
        active() &&
        traceEnum(issue, ['correlation_unavailable', 'external_client_side_unobservable'])
      )
        issues.add(issue);
    },
    snapshot() {
      if (overflow) return Object.freeze({ ok: false, reason: 'omission_counter_overflow' });
      return Object.freeze({
        ok: true,
        value: Object.freeze({
          serverAuctions: Object.freeze([...records]),
          slotCorrelations: Object.freeze([...sidecars]),
          issues: Object.freeze(TRACE_COVERAGE_ISSUES.filter((issue) => issues.has(issue))),
          omittedServerAuctions: omittedAuctions,
          omittedSlotCorrelations: omittedSidecars,
        }),
      });
    },
    captureStatus() {
      const failed = [
        'evidence_projection_failed',
        'evidence_transport_failed',
        'evidence_validation_failed',
        'record_evicted',
      ].some((issue) => issues.has(issue as TraceCoverageIssue));
      return records.length
        ? failed
          ? 'partial'
          : 'complete'
        : failed
          ? 'unavailable'
          : 'not_observed';
    },
    destroy() {
      destroyed = true;
      records = [];
      sidecars = [];
      issues.clear();
      omittedAuctions = 0;
      omittedSidecars = 0;
      overflow = false;
    },
  };
  return Object.freeze(api);
}
