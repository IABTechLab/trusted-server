import { joinTraceEvidence } from './correlation';
import { copyTraceReport, downloadTraceReport, shareTraceReport } from './export';
import { endTraceSessionAndObserve, type TraceSessionChangeResult } from './lifecycle';
import type { TraceGptRequestCycle, TraceReportV1, TraceStoredReportV1 } from './report-types';
import { mountTraceSetup } from './setup';
import { deleteTraceReport, readTraceReport } from './storage';
import type { CookieHealth, TraceRequestContextV1 } from './types';

export interface TraceViewerOptions {
  readonly origin?: string;
  readonly now?: () => number;
  readonly storage?: Pick<Storage, 'getItem' | 'setItem' | 'removeItem'>;
  readonly confirm?: (message: string) => boolean;
  readonly copy?: typeof copyTraceReport;
  readonly download?: typeof downloadTraceReport;
  readonly share?: typeof shareTraceReport;
}

function label(value: string): string {
  const labels: Record<string, string> = {
    no_bid: 'No bid returned',
    no_candidate: 'No candidate',
    selected: 'Candidate selected',
    selected_unrenderable: 'Selected candidate could not be rendered',
    trusted_server_direct: 'Trusted Server request path observed',
    prebid_refresh: 'Browser refresh observed; winner not determined',
    publisher_refresh: 'Browser refresh observed; winner not determined',
    competing: 'Multiple or unknown delivery paths',
    unattributed: 'Multiple or unknown delivery paths',
    trusted_server_response_sent: 'Trusted Server creative response sent',
    trusted_server_selected: 'Trusted Server candidate selected; render unconfirmed',
    candidate_unconfirmed: 'Candidate unconfirmed',
    not_observed: 'Not observed',
    unknown: 'Unknown',
    unavailable: 'Unavailable',
  };
  return (
    labels[value] ??
    value
      .replace(/([a-z])([A-Z])/g, '$1 $2')
      .replace(/_/g, ' ')
      .replace(/^./, (first) => first.toUpperCase())
  );
}
function valueText(value: unknown): string {
  if (value === undefined) return 'Unavailable';
  if (typeof value === 'boolean') return value ? 'Yes' : 'No';
  if (typeof value === 'number') return String(value);
  if (typeof value === 'string') return value;
  return 'Unavailable';
}
function healthText(health: CookieHealth): string {
  if (health.state === 'absent') return 'Not present in this request';
  if (health.state === 'present_valid') return 'Valid shape observed';
  if (health.state === 'duplicate') return 'Multiple values observed';
  if (health.state === 'present_invalid')
    return health.detail === 'oversized'
      ? 'Invalid shape — too long'
      : health.detail === 'unsupported_value'
        ? 'Invalid shape — unsupported value'
        : 'Invalid shape observed';
  if (health.detail === 'runtime_header_ambiguous')
    return 'Unavailable — runtime-visible cookies could not be reliably inspected';
  return health.detail === 'header_too_large'
    ? 'Unavailable — the visible cookie header was too large'
    : 'Unavailable — the visible cookie header was not valid text';
}
function element<K extends keyof HTMLElementTagNameMap>(
  root: Document,
  tag: K,
  text?: string
): HTMLElementTagNameMap[K] {
  const node = root.createElement(tag);
  if (text !== undefined) node.textContent = text;
  return node;
}
function section(root: Document, parent: Element, title: string, id?: string): HTMLElement {
  const node = element(root, 'section');
  if (id) node.id = id;
  node.append(element(root, 'h2', title));
  parent.append(node);
  return node;
}
function facts(
  root: Document,
  parent: Element,
  rows: readonly (readonly [string, unknown])[]
): void {
  const list = element(root, 'dl');
  for (const [name, value] of rows)
    list.append(element(root, 'dt', name), element(root, 'dd', valueText(value)));
  parent.append(list);
}
function sizes(value?: readonly (readonly [number, number])[]): string {
  return value === undefined
    ? 'Unavailable'
    : value.length === 0
      ? 'Not observed'
      : value.map(([width, height]) => `${width} × ${height}`).join(', ');
}
function millis(value?: number): string {
  return value === undefined ? 'Unavailable' : `${value} ms`;
}
const NETWORK_LABELS: Record<keyof TraceRequestContextV1['network'], string> = {
  masked_client_ip: 'Approximate network identifier',
  country: 'Country',
  region: 'Region',
  asn: 'ASN',
  http_version: 'HTTP version',
  tls_protocol: 'TLS protocol',
  tls_cipher: 'TLS cipher',
  edge_hostname: 'Edge hostname',
  edge_region: 'Edge region',
  edge_pop: 'Edge POP',
};
const CYCLE_FACTS: Record<
  Exclude<
    keyof TraceGptRequestCycle,
    | 'requestNumber'
    | 'durations'
    | 'requestedSlotSizes'
    | 'size'
    | 'observedSlotSize'
    | 'trustedServerAuctionId'
    | 'trustedServerCreativeFailures'
    | 'isEmpty'
    | 'requestPath'
  >,
  string
> = {
  requestedAtMs: 'Requested (browser clock)',
  responseAtMs: 'Response received (browser clock)',
  renderAtMs: 'Rendered (browser clock)',
  loadAtMs: 'Loaded (browser clock)',
  viewableAtMs: 'Viewable (browser clock)',
  isBackfill: 'Backfill observed',
  slotContentChanged: 'Slot content changed',
  incompleteSequence: 'Incomplete sequence',
  responseClass: 'GPT response class',
  requestIntentId: 'Browser request intent number',
  opportunityToRequestMs: 'Opportunity to request',
  replacedRequestNumber: 'Replaced request number',
  previousRenderToRequestMs: 'Previous render to request',
  creativeChanged: 'Creative changed',
  loadObservedBeforeRender: 'Load observed before render',
  trustedServerOpportunity: 'Trusted Server candidate opportunity',
  trustedServerCreativeRequestAtMs: 'Creative bridge request (browser clock)',
  trustedServerCreativeResponseAtMs: 'Creative bridge response (browser clock)',
  delivery: 'Creative delivery observation',
};

function renderReport(
  root: Document,
  report: TraceReportV1,
  origin: string,
  storedAtMs: number
): HTMLElement {
  const article = element(root, 'article');
  article.id = 'trace-report';
  article.append(
    element(root, 'h1', 'Trusted Server trace results'),
    element(root, 'p', 'Browser-carried, unverified diagnostic data')
  );
  article.append(
    element(
      root,
      'p',
      'This browser snapshot helps troubleshoot rendering. It is not proof of a server event, identity, or security incident.'
    )
  );
  const summary = section(root, article, 'Report summary');
  facts(root, summary, [
    ['Captured at', report.captured_at],
    ['Publisher origin', report.gpt_diagnostics.page.origin],
    ['Server auctions retained', report.server_auctions.length],
    ['GPT slots retained', report.gpt_diagnostics.slots.length],
  ]);
  const network = section(root, article, 'Publisher request');
  network.append(
    element(root, 'p', 'Produced by Trusted Server; copied through an untrusted browser snapshot')
  );
  network.append(
    element(
      root,
      'p',
      'These facts describe the traced publisher document. Masked identifiers are approximate and may still identify a network.'
    )
  );
  facts(root, network, [
    ['Document request captured at', report.request_context.captured_at],
    ...Object.entries(NETWORK_LABELS).map(
      ([key, name]) =>
        [
          name,
          report.request_context.network[key as keyof TraceRequestContextV1['network']],
        ] as const
    ),
  ]);
  const cookies = section(root, article, 'Cookie health', 'trace-report-cookies');
  cookies.append(
    element(
      root,
      'p',
      'Produced by Trusted Server; copied through an untrusted browser snapshot. Only cookie shape visible in this request is inspected; values and browser attributes are excluded.'
    )
  );
  facts(root, cookies, [
    ['Edge Cookie', healthText(report.request_context.cookies.ts_ec)],
    ['External IDs', healthText(report.request_context.cookies.ts_eids)],
    ['Tester', healthText(report.request_context.cookies.ts_tester)],
    ['Diagnostics session', healthText(report.request_context.cookies.diagnostics_session)],
  ]);
  const auctions = section(root, article, 'Server auctions');
  auctions.append(
    element(
      root,
      'p',
      'Returned bid counts may overlap between slots and must not be summed as unique bids.'
    )
  );
  const joined = joinTraceEvidence(report, origin, storedAtMs);
  if (!report.server_auctions.length)
    auctions.append(element(root, 'p', 'Not observed. This does not mean no server auction ran.'));
  for (const [index, view] of (joined?.auctions ?? []).entries()) {
    const entry = element(root, 'details');
    entry.open = true;
    entry.append(element(root, 'summary', `Auction ${index + 1}: ${view.sourceLabel}`));
    entry.append(
      element(root, 'p', 'Produced by Trusted Server; copied through an untrusted browser snapshot')
    );
    facts(root, entry, [
      ['Terminal outcome', label(view.evidence.terminal_status)],
      [
        'Terminal reason',
        view.evidence.terminal_reason === undefined
          ? 'Unavailable'
          : label(view.evidence.terminal_reason),
      ],
      ['Server auction-local elapsed time', millis(view.evidence.total_time_ms)],
      ['Request-relative milestones', view.relativeMilestonesLabel],
    ]);
    entry.append(element(root, 'p', view.providerScopeLabel));
    for (const provider of view.evidence.provider_calls)
      facts(root, entry, [
        [`Provider call ${provider.provider_number}`, label(provider.role)],
        ['Call outcome', label(provider.status)],
        ['Provider-local elapsed time', millis(provider.response_time_ms)],
        ['Returned bid count', provider.returned_bid_count],
      ]);
    if (!view.evidence.provider_calls.length)
      entry.append(element(root, 'p', 'Provider calls: Not observed'));
    for (const slot of view.slots) {
      const box = element(root, 'details');
      box.open = true;
      box.append(element(root, 'summary', `Server slot ${slot.serverSlot.slot_number}`));
      facts(root, box, [
        ['Requested sizes', sizes(slot.serverSlot.requested_sizes)],
        ['Returned bid count', slot.serverSlot.returned_bid_count],
        ['Candidate', label(slot.serverSlot.candidate)],
        [
          'Selected creative size',
          slot.serverSlot.selected_creative_size
            ? sizes([slot.serverSlot.selected_creative_size])
            : 'Unavailable',
        ],
        [
          'Correlation',
          slot.correlation === 'matched'
            ? `Matched GPT slot ${slot.runtimeSlotNumber}, request ${slot.requestNumber}`
            : 'Correlation unknown',
        ],
        ['Browser request path', slot.pathLabel ?? 'Unknown'],
        ['Creative participation', slot.creativeLabel ?? 'Participation unconfirmed'],
      ]);
      entry.append(box);
    }
    facts(root, entry, [
      ['Provider calls omitted', view.evidence.truncation.omitted_provider_calls],
      ['Server slots omitted', view.evidence.truncation.omitted_slots],
      ['Nested values omitted', view.evidence.truncation.omitted_nested_values],
    ]);
    auctions.append(entry);
  }
  const gpt = section(root, article, 'GPT delivery and creative rendering');
  gpt.append(
    element(
      root,
      'p',
      'Browser observed. Server auction → GPT request/response → creative render/load/viewability are separate observations. A filled slot does not identify an auction winner.'
    )
  );
  facts(root, gpt, [['Browser snapshot captured at', report.gpt_diagnostics.capturedAt]]);
  if (!report.gpt_diagnostics.slots.length) gpt.append(element(root, 'p', 'Not observed'));
  for (const slot of report.gpt_diagnostics.slots) {
    const box = element(root, 'details');
    box.open = true;
    box.append(element(root, 'summary', `GPT slot ${slot.runtimeSlotNumber}`));
    facts(root, box, [
      ['Binding', label(slot.binding.status)],
      [
        'Binding reason',
        slot.binding.reason === undefined ? 'Unavailable' : label(slot.binding.reason),
      ],
      ['Current visibility percentage', slot.currentVisibilityPercentage],
      ['Maximum visibility percentage', slot.maximumVisibilityPercentage],
    ]);
    if (!slot.requests.length) box.append(element(root, 'p', 'Requests: Not observed'));
    for (const cycle of slot.requests) {
      const request = element(root, 'details');
      request.open = true;
      request.append(element(root, 'summary', `Request ${cycle.requestNumber}`));
      const matches = (joined?.auctions ?? [])
        .flatMap((auction) => auction.slots)
        .filter(
          (entry) =>
            entry.correlation === 'matched' &&
            entry.runtimeSlotNumber === slot.runtimeSlotNumber &&
            entry.requestNumber === cycle.requestNumber
        );
      facts(root, request, [
        ['Correlation', matches.length === 1 ? 'Matched server slot' : 'Correlation unknown'],
        [
          'Creative participation',
          matches.length === 1 ? matches[0].creativeLabel : 'Participation unconfirmed',
        ],
        [
          'GPT fill observation',
          cycle.isEmpty === undefined ? 'Unknown' : cycle.isEmpty ? 'Empty' : 'Filled',
        ],
        [
          'Browser request path',
          cycle.requestPath === undefined ? 'Unavailable' : label(cycle.requestPath),
        ],
        ['Requested sizes', sizes(cycle.requestedSlotSizes)],
        ['Rendered size', cycle.size ? sizes([cycle.size]) : 'Unavailable'],
        [
          'Observed CSS box size',
          cycle.observedSlotSize ? sizes([cycle.observedSlotSize]) : 'Unavailable',
        ],
      ]);
      const rows = Object.entries(CYCLE_FACTS).map(([key, name]): readonly [string, unknown] => {
        const value = cycle[key as keyof typeof CYCLE_FACTS];
        return [
          name,
          typeof value === 'string'
            ? label(value)
            : key.endsWith('Ms')
              ? millis(value as number | undefined)
              : value,
        ];
      });
      facts(root, request, rows);
      facts(root, request, [
        ['Request to response', millis(cycle.durations.requestToResponseMs)],
        ['Response to render', millis(cycle.durations.responseToRenderMs)],
        ['Request to render', millis(cycle.durations.requestToRenderMs)],
        ['Render to load', millis(cycle.durations.renderToLoadMs)],
        ['Render to viewable', millis(cycle.durations.renderToViewableMs)],
        [
          'Creative bridge failures',
          cycle.trustedServerCreativeFailures === undefined
            ? 'Unavailable'
            : cycle.trustedServerCreativeFailures.length === 0
              ? 'Not observed'
              : cycle.trustedServerCreativeFailures.map(label).join(', '),
        ],
      ]);
      box.append(request);
    }
    gpt.append(box);
  }
  const coverage = section(root, article, 'Coverage and ambiguity');
  facts(root, coverage, [
    ['Server capture', label(report.auction_coverage.capture_status)],
    [
      'Capture and interpretation limits',
      report.auction_coverage.issues.length
        ? report.auction_coverage.issues.map(label).join(', ')
        : 'None recorded',
    ],
    ['Correlation sidecars retained', report.slot_correlations.length],
  ]);
  for (const [kind, counts] of Object.entries(report.gpt_diagnostics.coverage))
    facts(root, coverage, [
      [
        label(kind),
        `${counts.observed} observed; ${counts.matched} matched; ${counts.unmatched} unmatched; ${counts.ambiguous} ambiguous`,
      ],
    ]);
  for (const [key, count] of Object.entries(report.truncation))
    facts(root, coverage, [[label(key), count]]);
  for (const [key, count] of Object.entries(report.gpt_diagnostics.metadata))
    facts(root, coverage, [[`GPT ${label(key)}`, count]]);
  for (const issue of report.gpt_diagnostics.callbackIssues)
    facts(root, coverage, [
      ['Callback issue', label(issue.kind)],
      ['GPT slot', issue.runtimeSlotNumber],
      ['Browser clock', millis(issue.timestampMs)],
      ['Disposition', label(issue.disposition)],
      ['Reason', label(issue.reason)],
    ]);
  for (const issue of report.gpt_diagnostics.attributionIssues ?? [])
    facts(root, coverage, [
      ['Creative attribution issue', label(issue.reason)],
      ['GPT slot', issue.runtimeSlotNumber],
      ['Browser clock', millis(issue.timestampMs)],
    ]);
  for (const [index, record] of report.slot_correlations.entries()) {
    const entry = element(root, 'details');
    entry.append(element(root, 'summary', `Correlation record ${index + 1}`));
    entry.append(
      element(
        root,
        'p',
        'Browser observed correlation. These opaque references permit a join only when unique and consistent; duplicate or conflicting records remain unknown.'
      )
    );
    facts(root, entry, [
      ['Auction reference', record.diagnostic_auction_id],
      ['Slot reference', record.slot_ref],
      ['GPT slot number', record.runtime_slot_number],
      ['GPT request number', record.request_number],
    ]);
    coverage.append(entry);
  }
  return article;
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
  cleanup.append(localStatus);
  const serverStatus = element(
    root,
    'p',
    'Server tracing state has not been changed by this report visit.'
  );
  serverStatus.id = 'trace-cleanup-server-status';
  serverStatus.setAttribute('role', 'status');
  serverStatus.setAttribute('aria-live', 'polite');
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
      stored = undefined;
      article.remove();
      deleteButton.hidden = true;
      localStatus.textContent = 'Local report deleted from this tab.';
    } else
      localStatus.textContent =
        'Local report deletion failed. The report remains displayed; retry deletion.';
  };
  let ending = false;
  const end = async (): Promise<void> => {
    if (destroyed || ending) return;
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
