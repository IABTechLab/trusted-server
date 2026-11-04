import { parseTraceReport } from './report-validation';

/** One deterministic file name for every trace export surface. */
export const TRACE_REPORT_FILENAME = 'trusted-server-trace-v1.json';
type TraceExportFailure = { readonly status: 'failed' | 'unsupported' | 'invalid_report' };

/** Formats the validated public report identically for Copy, Share, and Download. */
export function formatTraceReport(
  value: unknown,
  origin: string,
  capturedAtMs: number
): string | undefined {
  const report = parseTraceReport(value, origin, capturedAtMs);
  return report ? JSON.stringify(report, null, 2) : undefined;
}

/** Starts one explicit download and independently defers its object URL cleanup. */
export function downloadTraceReport(
  value: unknown,
  origin: string,
  capturedAtMs: number
): { readonly status: 'downloaded' } | TraceExportFailure {
  const json = formatTraceReport(value, origin, capturedAtMs);
  if (json === undefined) return { status: 'invalid_report' };
  let url: string;
  try {
    url = URL.createObjectURL(new Blob([json], { type: 'application/json' }));
  } catch {
    return { status: 'failed' };
  }
  let anchor: HTMLAnchorElement | undefined;
  try {
    anchor = document.createElement('a');
    anchor.href = url;
    anchor.download = TRACE_REPORT_FILENAME;
    document.body.append(anchor);
    anchor.click();
    return { status: 'downloaded' };
  } catch {
    return { status: 'failed' };
  } finally {
    try {
      anchor?.remove();
    } catch {
      /* URL cleanup still runs when DOM removal is unavailable. */
    }
    window.setTimeout(() => URL.revokeObjectURL(url), 1000);
  }
}

/** Copies validated report JSON only when the user invokes this action. */
export async function copyTraceReport(
  value: unknown,
  origin: string,
  capturedAtMs: number
): Promise<{ readonly status: 'copied' } | TraceExportFailure> {
  const json = formatTraceReport(value, origin, capturedAtMs);
  if (json === undefined) return { status: 'invalid_report' };
  try {
    if (typeof navigator.clipboard?.writeText !== 'function') return { status: 'unsupported' };
    await navigator.clipboard.writeText(json);
    return { status: 'copied' };
  } catch {
    return { status: 'failed' };
  }
}

/** Shares the same validated JSON file when file sharing is supported. */
export async function shareTraceReport(
  value: unknown,
  origin: string,
  capturedAtMs: number
): Promise<{ readonly status: 'shared' } | TraceExportFailure> {
  const json = formatTraceReport(value, origin, capturedAtMs);
  if (json === undefined) return { status: 'invalid_report' };
  try {
    if (typeof navigator.canShare !== 'function' || typeof navigator.share !== 'function')
      return { status: 'unsupported' };
    const file = new File([json], TRACE_REPORT_FILENAME, { type: 'application/json' });
    const data: ShareData = { files: [file] };
    if (!navigator.canShare(data)) return { status: 'unsupported' };
    await navigator.share(data);
    return { status: 'shared' };
  } catch {
    return { status: 'failed' };
  }
}
