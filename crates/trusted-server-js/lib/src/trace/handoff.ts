import type { TsjsApi } from '../core/types';

import { downloadTraceReport } from './export';
import { buildTraceReport } from './report';
import type { TraceStoredReportV1 } from './report-types';
import { storeTraceReport } from './storage';

export interface TraceHandoffState {
  readonly kind:
    | 'ready'
    | 'capturing'
    | 'capture_failed'
    | 'storage_unavailable'
    | 'navigation_unavailable'
    | 'navigating'
    | 'download_failed'
    | 'downloaded';
  readonly downloadAvailable: boolean;
}
export interface TraceHandoffOptions {
  readonly target: {
    __tsjs_trace_active?: unknown;
    __tsjs_trace_request_context?: unknown;
    tsjs?: TsjsApi;
    location: Pick<Location, 'origin' | 'assign'>;
    sessionStorage: Pick<Storage, 'getItem' | 'setItem' | 'removeItem'>;
  };
  readonly now?: () => number;
  readonly onChange?: (state: TraceHandoffState) => void;
  readonly download?: typeof downloadTraceReport;
}
export interface TraceHandoff {
  view(): void;
  download(): void;
  destroy(): void;
}

/** Captures one explicit combined report and navigates only after its validated save. */
export function createTraceHandoff(options: TraceHandoffOptions): TraceHandoff | undefined {
  try {
    if (options.target.__tsjs_trace_active !== true) return undefined;
  } catch {
    return undefined;
  }
  let destroyed = false;
  let busy = false;
  let recovery: TraceStoredReportV1 | undefined;
  let recoveryOrigin: string | undefined;
  const notify = (kind: TraceHandoffState['kind']): void => {
    if (destroyed) return;
    try {
      options.onChange?.(Object.freeze({ kind, downloadAvailable: recovery !== undefined }));
    } catch {
      /* UI callbacks cannot interrupt the capture boundary. */
    }
  };
  const action: TraceHandoff = {
    view() {
      if (destroyed || busy) return;
      busy = true;
      recovery = undefined;
      recoveryOrigin = undefined;
      notify('capturing');
      try {
        if (destroyed) return;
        if (options.target.__tsjs_trace_active !== true) {
          notify('capture_failed');
          return;
        }
        const clock = (options.now ?? Date.now)();
        const origin = options.target.location.origin;
        const api = options.target.tsjs;
        if (typeof api?.gptDiagnostics?.snapshot !== 'function' || !api.traceEvidence) {
          notify('capture_failed');
          return;
        }
        const collector = api.traceEvidence.snapshot();
        if (destroyed) return;
        if (!collector.ok) {
          notify('capture_failed');
          return;
        }
        const result = buildTraceReport({
          capturedAtMs: clock,
          origin,
          requestContext: options.target.__tsjs_trace_request_context,
          gptSource: api.gptDiagnostics.snapshot(),
          collector: collector.value,
        });
        if (destroyed) return;
        if (!result.ok) {
          notify('capture_failed');
          return;
        }
        recovery = result.value;
        recoveryOrigin = origin;
        let stored = false;
        try {
          stored =
            storeTraceReport(recovery, origin, clock, options.target.sessionStorage).status ===
            'stored';
        } catch {
          /* Unavailable storage preserves this valid combined recovery report. */
        }
        if (destroyed) return;
        if (!stored) {
          notify('storage_unavailable');
          return;
        }
        try {
          options.target.location.assign('/_ts/trace');
          notify('navigating');
        } catch {
          notify('navigation_unavailable');
        }
      } catch {
        recovery = undefined;
        recoveryOrigin = undefined;
        notify('capture_failed');
      } finally {
        busy = false;
      }
    },
    download() {
      if (destroyed || busy || !recovery || !recoveryOrigin) return;
      try {
        const result = (options.download ?? downloadTraceReport)(
          recovery.report,
          recoveryOrigin,
          recovery.stored_at_ms
        );
        notify(result.status === 'downloaded' ? 'downloaded' : 'download_failed');
      } catch {
        notify('download_failed');
      }
    },
    destroy() {
      destroyed = true;
      recovery = undefined;
      recoveryOrigin = undefined;
    },
  };
  return Object.freeze(action);
}
