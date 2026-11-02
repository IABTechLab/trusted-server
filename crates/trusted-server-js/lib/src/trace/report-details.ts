import type { TraceEvidenceView } from './correlation';
import {
  capturedLabel,
  collapsible,
  element,
  facts,
  healthText,
  label,
  millis,
  section,
  sizes,
} from './report-dom';
import type { TraceGptRequestCycle, TraceReportV1 } from './report-types';
import type { TraceRequestContextV1 } from './types';

/** Collapsed detail sections that keep every retained trace field reachable. */
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

export function renderRequestDetails(
  root: Document,
  parent: HTMLElement,
  runtimeSlotNumber: number,
  cycle: TraceGptRequestCycle,
  joined: TraceEvidenceView | undefined
): void {
  const request = element(root, 'details');
  request.className = 'trace-request';
  request.append(element(root, 'summary', `Request ${cycle.requestNumber}`));
  const matches = (joined?.auctions ?? [])
    .flatMap((auction) => auction.slots)
    .filter(
      (entry) =>
        entry.correlation === 'matched' &&
        entry.runtimeSlotNumber === runtimeSlotNumber &&
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
  parent.append(request);
}

export function renderAuctions(
  root: Document,
  article: HTMLElement,
  report: TraceReportV1,
  joined: TraceEvidenceView | undefined
): void {
  // Stays expanded: auction outcomes are primary evidence, not optional detail.
  const auctions = section(root, article, 'Server auctions', 'trace-report-auctions');
  auctions.append(
    element(
      root,
      'p',
      'Returned bid counts may overlap between slots and must not be summed as unique bids.'
    )
  );
  if (!report.server_auctions.length)
    auctions.append(element(root, 'p', 'Not observed. This does not mean no server auction ran.'));
  for (const [index, view] of (joined?.auctions ?? []).entries()) {
    const entry = element(root, 'details');
    entry.className = 'trace-auction';
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
      const box = element(root, 'div');
      box.className = 'trace-server-slot';
      box.append(element(root, 'h3', `Server slot ${slot.serverSlot.slot_number}`));
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
}

export function renderRequestContext(
  root: Document,
  article: HTMLElement,
  report: TraceReportV1
): void {
  const network = collapsible(root, article, 'Publisher request', 'trace-report-request');
  network.append(
    element(root, 'p', 'Produced by Trusted Server; copied through an untrusted browser snapshot'),
    element(
      root,
      'p',
      'These facts describe the traced publisher document. Masked identifiers are approximate and may still identify a network.'
    )
  );
  facts(root, network, [
    ['Document request captured at', capturedLabel(report.request_context.captured_at)],
    ...Object.entries(NETWORK_LABELS).map(
      ([key, name]) =>
        [
          name,
          report.request_context.network[key as keyof TraceRequestContextV1['network']],
        ] as const
    ),
  ]);
  const cookies = collapsible(root, article, 'Cookie health', 'trace-report-cookies');
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
}

export function renderCoverage(root: Document, article: HTMLElement, report: TraceReportV1): void {
  const coverage = collapsible(root, article, 'Coverage and ambiguity', 'trace-report-coverage');
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
}
