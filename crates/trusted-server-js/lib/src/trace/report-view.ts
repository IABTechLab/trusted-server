import { joinTraceEvidence, type TraceEvidenceView } from './correlation';
import { copyTraceReport, downloadTraceReport, shareTraceReport } from './export';
import { endTraceSessionAndObserve, type TraceSessionChangeResult } from './lifecycle';
import type { TraceReportV1, TraceStoredReportV1 } from './report-types';
import { mountTraceSetup } from './setup';
import { deleteTraceReport, readTraceReport } from './storage';
import {
  renderAuctions,
  renderCoverage,
  renderRequestContext,
  renderRequestDetails,
} from './report-details';
import { capturedLabel, element, facts, label, millis, section, sizes } from './report-dom';
import {
  summarizeTraceReport,
  type TraceSlotCard,
  type TraceSlotFill,
  type TraceSummary,
} from './view-model';

export interface TraceViewerOptions {
  readonly origin?: string;
  readonly now?: () => number;
  readonly storage?: Pick<Storage, 'getItem' | 'setItem' | 'removeItem'>;
  readonly confirm?: (message: string) => boolean;
  readonly copy?: typeof copyTraceReport;
  readonly download?: typeof downloadTraceReport;
  readonly share?: typeof shareTraceReport;
}

const FILL_LABELS: Record<TraceSlotFill, string> = {
  filled: 'Filled',
  empty: 'Empty',
  unknown: 'Fill unknown',
};
const FILL_COUNT_LABELS: Record<TraceSlotFill, string> = {
  filled: 'filled',
  empty: 'empty',
  unknown: 'fill unknown',
};

function chip(root: Document, fill: TraceSlotFill, text: string): HTMLElement {
  const node = element(root, 'span', text);
  node.className = `trace-chip trace-chip-${fill}`;
  return node;
}

function renderSummary(root: Document, article: HTMLElement, summary: TraceSummary): void {
  const box = section(root, article, 'What happened', 'trace-report-summary');
  const verdict = element(root, 'div');
  verdict.className = `trace-verdict ${summary.needsAttention ? 'is-attention' : 'is-clear'}`;
  const headline = element(root, 'p', summary.headline);
  headline.className = 'trace-headline';
  verdict.append(headline);
  for (const sentence of summary.reading) verdict.append(element(root, 'p', sentence));
  const stats = element(root, 'ul');
  stats.className = 'trace-stats';
  for (const stat of summary.stats) {
    const item = element(root, 'li');
    const value = element(root, 'span', String(stat.value));
    value.className = 'trace-stat-value';
    const label = element(root, 'span', stat.label);
    label.className = 'trace-stat-label';
    item.append(value, label);
    stats.append(item);
  }
  verdict.append(stats);
  if (summary.fills.length) {
    const fills = element(root, 'p');
    fills.className = 'trace-chips';
    for (const entry of summary.fills)
      fills.append(chip(root, entry.fill, `${entry.count} ${FILL_COUNT_LABELS[entry.fill]}`));
    verdict.append(fills);
  }
  box.append(verdict);
  const flagged = summary.slots.filter((slot) => slot.attention.length > 0);
  if (!flagged.length) return;
  const attention = section(root, article, 'Needs attention', 'trace-report-attention');
  const list = element(root, 'ul');
  list.className = 'trace-attention';
  for (const slot of flagged) {
    const item = element(root, 'li');
    const link = element(root, 'a', `Slot ${slot.runtimeSlotNumber}`);
    link.href = `#trace-slot-${slot.runtimeSlotNumber}`;
    item.append(link, element(root, 'span', ` — ${slot.attention.join('; ')}`));
    list.append(item);
  }
  attention.append(list);
}

function renderTiming(root: Document, parent: HTMLElement, card: TraceSlotCard): void {
  if (!card.timing.length) {
    parent.append(element(root, 'p', 'Request timing unavailable'));
    return;
  }
  const description = card.timing.map((segment) => `${segment.label} ${millis(segment.ms)}`);
  const bar = element(root, 'div');
  bar.className = 'trace-timing';
  bar.setAttribute('role', 'img');
  bar.setAttribute(
    'aria-label',
    `Request ${card.timingRequestNumber ?? 1} timing: ${description.join(', ')}`
  );
  for (const segment of card.timing) {
    const part = element(root, 'span');
    part.className = `trace-segment trace-segment-${segment.kind} trace-weight-${segment.weight}`;
    bar.append(part);
  }
  const legend = element(root, 'ul');
  legend.className = 'trace-legend';
  legend.setAttribute('aria-hidden', 'true');
  for (const segment of card.timing) {
    const item = element(root, 'li', `${segment.label} ${millis(segment.ms)}`);
    item.className = `trace-legend-${segment.kind}`;
    legend.append(item);
  }
  parent.append(bar, legend);
}

function renderSlots(
  root: Document,
  article: HTMLElement,
  report: TraceReportV1,
  summary: TraceSummary,
  joined: TraceEvidenceView | undefined
): void {
  const slots = section(root, article, 'Ad slots', 'trace-report-slots');
  slots.append(
    element(
      root,
      'p',
      'Browser observed. Server auction → GPT request/response → creative render/load/viewability are separate observations. A filled slot does not identify an auction winner.'
    )
  );
  facts(root, slots, [
    ['Browser snapshot captured at', capturedLabel(report.gpt_diagnostics.capturedAt)],
  ]);
  if (!report.gpt_diagnostics.slots.length) slots.append(element(root, 'p', 'Not observed'));
  for (const [index, slot] of report.gpt_diagnostics.slots.entries()) {
    const card = summary.slots[index];
    const node = element(root, 'article');
    node.className = 'trace-slot';
    node.id = `trace-slot-${slot.runtimeSlotNumber}`;
    const header = element(root, 'header');
    header.append(
      element(
        root,
        'h3',
        card.size
          ? `Slot ${slot.runtimeSlotNumber} · ${sizes([card.size])}`
          : `Slot ${slot.runtimeSlotNumber}`
      ),
      chip(
        root,
        card.fill,
        card.backfill && card.fill === 'filled' ? 'Filled · backfill' : FILL_LABELS[card.fill]
      )
    );
    node.append(header);
    renderTiming(root, node, card);
    const link = element(
      root,
      'p',
      card.link
        ? `Linked to auction ${card.link.auctionNumber} (${card.link.sourceLabel}) · server slot ${card.link.serverSlotNumber} · ${label(card.link.candidate)}`
        : 'Not linked to a server auction'
    );
    link.className = card.link ? 'trace-link' : 'trace-link is-unlinked';
    node.append(link);
    if (card.requestCount > 1)
      node.append(
        element(
          root,
          'p',
          `${card.requestCount} requests; timing shows request ${card.timingRequestNumber}.`
        )
      );
    const details = element(root, 'details');
    details.className = 'trace-slot-details';
    details.append(element(root, 'summary', `All details for slot ${slot.runtimeSlotNumber}`));
    facts(root, details, [
      ['Binding', label(slot.binding.status)],
      [
        'Binding reason',
        slot.binding.reason === undefined ? 'Unavailable' : label(slot.binding.reason),
      ],
      ['Current visibility percentage', slot.currentVisibilityPercentage],
      ['Maximum visibility percentage', slot.maximumVisibilityPercentage],
    ]);
    if (!slot.requests.length) details.append(element(root, 'p', 'Requests: Not observed'));
    for (const cycle of slot.requests)
      renderRequestDetails(root, details, slot.runtimeSlotNumber, cycle, joined);
    node.append(details);
    slots.append(node);
  }
}

function renderReport(
  root: Document,
  report: TraceReportV1,
  origin: string,
  storedAtMs: number
): HTMLElement {
  const article = element(root, 'article');
  article.id = 'trace-report';
  const notice = element(root, 'p', 'Browser-carried, unverified diagnostic data');
  notice.className = 'trace-notice';
  const header = element(root, 'header');
  header.className = 'trace-report-header';
  const meta = element(
    root,
    'p',
    `Captured ${capturedLabel(report.captured_at)} · ${report.gpt_diagnostics.page.origin} · ${plainCount(report.gpt_diagnostics.slots.length, 'ad slot', 'ad slots')} · ${plainCount(report.server_auctions.length, 'server auction', 'server auctions')}`
  );
  meta.className = 'trace-meta';
  header.append(
    notice,
    element(root, 'h1', 'Trusted Server trace results'),
    meta,
    element(
      root,
      'p',
      'This browser snapshot helps troubleshoot rendering. It is not proof of a server event, identity, or security incident.'
    )
  );
  article.append(header);
  const joined = joinTraceEvidence(report, origin, storedAtMs);
  const summary = summarizeTraceReport(report, joined);
  renderSummary(root, article, summary);
  renderSlots(root, article, report, summary, joined);
  // A visual divider, not a heading: the sections below keep their own h2 outline.
  const more = element(root, 'p', 'More detail');
  more.className = 'trace-more';
  article.append(more);
  renderAuctions(root, article, report, joined);
  renderRequestContext(root, article, report);
  renderCoverage(root, article, report);
  return article;
}

function plainCount(count: number, one: string, many: string): string {
  return `${count} ${count === 1 ? one : many}`;
}

/** Mounts the trace page using the current tab's validated report and setup controls. */
export function mountTraceViewer(
  root: Document = document,
  options: TraceViewerOptions = {}
): { destroy(): void } {
  const main = root.querySelector('main');
  if (!main) return { destroy() {} };
  const destroySetup = mountTraceSetup(root);
  const origin = options.origin ?? window.location.origin;
  let stored: TraceStoredReportV1 | undefined;
  let destroyed = false;
  const disposers: Array<() => void> = [];
  const control = (parent: Element, text: string, action: () => void): HTMLButtonElement => {
    const button = element(root, 'button', text);
    button.type = 'button';
    const listener = (): void => {
      if (!destroyed) action();
    };
    button.addEventListener('click', listener);
    disposers.push(() => button.removeEventListener('click', listener));
    parent.append(button);
    return button;
  };
  const destroy = (): void => {
    destroyed = true;
    stored = undefined;
    destroySetup();
    for (const dispose of disposers) dispose();
  };
  let read: ReturnType<typeof readTraceReport>;
  try {
    read = readTraceReport(origin, (options.now ?? Date.now)(), options.storage);
  } catch {
    read = { status: 'unavailable' };
  }
  if (read.status !== 'ready') {
    const notice = element(
      root,
      'p',
      read.status === 'absent'
        ? 'No saved report. Enable tracing, return to the affected page, reload once, reproduce the problem, then select View trace results.'
        : 'The saved report is unavailable, expired, or unsupported. Return to the affected page in this same tab and exact hostname, reload once, reproduce the problem, then select View trace results.'
    );
    notice.id = 'trace-report-notice';
    main.prepend(notice);
    return { destroy };
  }
  stored = read.value;
  const setup = element(root, 'details');
  setup.id = 'trace-viewer-setup';
  setup.append(element(root, 'summary', 'Setup request and tracing controls'));
  for (const child of Array.from(main.children)) setup.append(child);
  const article = renderReport(root, stored.report, origin, stored.stored_at_ms);
  main.append(article);
  const exports = section(root, article, 'Export');
  exports.append(
    element(
      root,
      'p',
      'Copy, Download and Share use the same public report JSON. The selected app receives this JSON when you choose Share. Nothing is uploaded by this viewer.'
    )
  );
  const exportControls = element(root, 'div');
  exportControls.className = 'controls';
  exports.append(exportControls);
  const exportStatus = element(root, 'p');
  exportStatus.id = 'trace-export-status';
  exportStatus.setAttribute('role', 'status');
  exportStatus.setAttribute('aria-live', 'polite');
  exports.append(exportStatus);
  let exporting = false;
  const exportReport = async (kind: 'Copy' | 'Download' | 'Share'): Promise<void> => {
    if (!stored || destroyed || exporting) return;
    exporting = true;
    const captured = stored;
    try {
      const action =
        kind === 'Copy'
          ? (options.copy ?? copyTraceReport)
          : kind === 'Share'
            ? (options.share ?? shareTraceReport)
            : (options.download ?? downloadTraceReport);
      const result = await action(captured.report, origin, captured.stored_at_ms);
      if (destroyed || stored !== captured) return;
      exportStatus.textContent =
        result.status === 'copied'
          ? 'Copied JSON.'
          : result.status === 'downloaded'
            ? 'Download started; completion is managed by your browser.'
            : result.status === 'shared'
              ? 'The share request completed.'
              : kind === 'Share' && result.status === 'unsupported'
                ? 'File sharing is unavailable. Use Copy or Download.'
                : `${kind} could not be completed. Your report remains available; retry or choose another export.`;
    } catch {
      if (!destroyed && stored === captured)
        exportStatus.textContent = `${kind} could not be completed. Your report remains available.`;
    } finally {
      exporting = false;
    }
  };
  for (const kind of ['Copy', 'Download', 'Share'] as const)
    control(exportControls, kind, () => {
      void exportReport(kind);
    });
  const cleanup = section(root, main, 'Report cleanup');
  const controls = element(root, 'div');
  controls.className = 'controls';
  cleanup.append(controls);
  const localStatus = element(root, 'p', 'A local report is saved in this tab.');
  localStatus.id = 'trace-cleanup-local-status';
  localStatus.setAttribute('role', 'status');
  localStatus.setAttribute('aria-live', 'polite');
  localStatus.tabIndex = -1;
  cleanup.append(localStatus);
  const serverStatus = element(
    root,
    'p',
    'Server tracing state has not been changed by this report visit.'
  );
  serverStatus.id = 'trace-cleanup-server-status';
  serverStatus.setAttribute('role', 'status');
  serverStatus.setAttribute('aria-live', 'polite');
  serverStatus.tabIndex = -1;
  cleanup.append(serverStatus);
  const removeLocal = (): void => {
    let result: ReturnType<typeof deleteTraceReport>;
    try {
      result = deleteTraceReport(options.storage);
    } catch {
      result = { status: 'unavailable' };
    }
    if (destroyed) return;
    if (result.status === 'deleted') {
      const losingFocus =
        root.activeElement === deleteButton || article.contains(root.activeElement);
      stored = undefined;
      article.remove();
      deleteButton.hidden = true;
      localStatus.textContent = 'Local report deleted from this tab.';
      if (losingFocus) localStatus.focus();
    } else
      localStatus.textContent =
        'Local report deletion failed. The report remains displayed; retry deletion.';
  };
  let ending = false;
  const end = async (): Promise<void> => {
    if (destroyed || ending) return;
    const retryHadFocus = root.activeElement === retryButton;
    ending = true;
    clearButton.disabled = retryButton.disabled = true;
    serverStatus.textContent = 'Requesting tracing end and checking the next request…';
    let result: TraceSessionChangeResult;
    try {
      result = await endTraceSessionAndObserve();
    } catch {
      result = { mutation: 'failed', observation: 'failed', confirmed: false };
    }
    if (destroyed) return;
    serverStatus.textContent = result.confirmed
      ? 'Tracing is off — no valid diagnostics session observed.'
      : result.observation === 'inactive'
        ? 'End tracing unconfirmed. No valid diagnostics session was observed; tracing may remain active. Retry end tracing.'
        : result.observation === 'active'
          ? 'End tracing unconfirmed — a valid session is still observed. Tracing may remain active. Retry end tracing.'
          : 'End tracing unconfirmed. Tracing may remain active. Retry end tracing.';
    retryButton.hidden = result.confirmed;
    clearButton.disabled = retryButton.disabled = false;
    if (
      result.confirmed &&
      retryHadFocus &&
      (root.activeElement === retryButton || root.activeElement === root.body)
    )
      serverStatus.focus();
    ending = false;
  };
  const clearButton = control(controls, 'Clear report and end tracing', () => {
    if (ending) return;
    let confirmed = false;
    try {
      confirmed = (options.confirm ?? ((message) => window.confirm(message)))(
        'Delete the report from this tab and request tracing end?'
      );
    } catch {
      /* No consent means no cleanup mutation. */
    }
    if (!confirmed || destroyed) return;
    removeLocal();
    if (!destroyed) void end();
  });
  const deleteButton = control(controls, 'Delete local report', removeLocal);
  const retryButton = control(controls, 'Retry end tracing', () => {
    void end();
  });
  retryButton.hidden = true;
  main.append(setup);
  return { destroy };
}
