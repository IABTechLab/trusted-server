import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { mountTraceViewer } from '../../src/trace/report-view';
import { TRACE_REPORT_STORAGE_KEY } from '../../src/trace/storage';
import { formatTraceReport } from '../../src/trace/export';
import type {
  copyTraceReport,
  downloadTraceReport,
  shareTraceReport,
} from '../../src/trace/export';

import { reportFixture, TRACE_NOW, TRACE_ORIGIN, AUCTION_TOKEN, SLOT_TOKEN } from './fixtures';

function page() {
  document.body.innerHTML =
    '<main><h1>Trusted Server ad diagnostics</h1><section><h2>Setup request</h2><p id="trace-session-state" data-observed-active="false"></p><p id="trace-status"></p><button id="trace-enable">Enable tracing</button><button id="trace-end">End tracing</button><button id="trace-back">Return to previous page</button><pre id="trace-request-context"></pre><dl id="trace-network-facts"></dl><dl id="trace-cookie-facts"></dl></section></main>';
  document.getElementById('trace-request-context')!.textContent = JSON.stringify({
    ...reportFixture().request_context,
    network: { edge_hostname: 'setup-only.example.com' },
  });
}
function setup(value: unknown = reportFixture()) {
  let serialized: string | null =
    value === null ? null : JSON.stringify({ stored_at_ms: TRACE_NOW, report: value });
  const storage = {
    getItem: vi.fn(() => serialized),
    setItem: vi.fn(),
    removeItem: vi.fn((_key: string) => {
      serialized = null;
    }),
  };
  const copy = vi.fn<typeof copyTraceReport>(async () => ({ status: 'copied' }));
  const download = vi.fn<typeof downloadTraceReport>(() => ({ status: 'downloaded' }));
  const share = vi.fn<typeof shareTraceReport>(async () => ({ status: 'shared' }));
  const options = {
    storage,
    now: () => TRACE_NOW,
    origin: TRACE_ORIGIN,
    confirm: vi.fn(() => true),
    copy,
    download,
    share,
  };
  return { storage, options, copy, download, share };
}
function button(label: string): HTMLButtonElement {
  const result = Array.from(document.querySelectorAll('button')).find(
    (candidate) => candidate.textContent === label
  );
  if (!result) throw new Error(`should find ${label} control`);
  return result;
}
function joinedReport() {
  const report = {
    ...reportFixture(),
    server_auctions: [
      {
        schema_version: 1,
        diagnostic_auction_id: AUCTION_TOKEN,
        source: 'initial_navigation_ssat',
        terminal_status: 'completed',
        total_time_ms: 10,
        provider_calls: [
          { provider_number: 1, role: 'bidder', status: 'no_bid', returned_bid_count: 0 },
        ],
        slots: [
          {
            slot_number: 1,
            slot_ref: SLOT_TOKEN,
            requested_sizes: [[300, 250]],
            returned_bid_count: 0,
            candidate: 'no_candidate',
          },
        ],
        truncation: { omitted_provider_calls: 0, omitted_slots: 0, omitted_nested_values: 0 },
        coverage: { provider_to_slot_no_bid: 'unavailable' },
      },
    ],
    slot_correlations: [
      {
        schema_version: 1,
        diagnostic_auction_id: AUCTION_TOKEN,
        slot_ref: SLOT_TOKEN,
        runtime_slot_number: 1,
        request_number: 1,
      },
    ],
    auction_coverage: { capture_status: 'complete', issues: [] },
  };
  return report;
}
const request = vi.fn<typeof fetch>();
beforeEach(() => {
  page();
  request.mockReset();
  vi.stubGlobal('fetch', request);
});
afterEach(() => {
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
  document.body.replaceChildren();
});

describe('consolidated trace report viewer', () => {
  it('keeps setup read-only when no report exists', () => {
    const fixture = setup(null);
    mountTraceViewer(document, fixture.options);
    expect(document.getElementById('trace-report')).toBeNull();
    expect(document.body.textContent).toContain('No saved report');
    expect(document.body.textContent).toContain('setup-only.example.com');
    expect(request).not.toHaveBeenCalled();
    expect(fixture.storage.setItem).not.toHaveBeenCalled();
  });
  it.each(['expired', 'hostile', 'unsupported', 'wrong-origin'])(
    'ignores and removes a %s report with actionable reproduction guidance',
    (reason) => {
      const report = reportFixture();
      if (reason === 'hostile')
        Object.assign(report, { private_value: '<img src="https://private.example.com">' });
      if (reason === 'unsupported') report.schema_version = 2;
      if (reason === 'wrong-origin')
        report.gpt_diagnostics.page.origin = 'https://other.example.com';
      const fixture = setup(report);
      if (reason === 'expired') fixture.options.now = () => TRACE_NOW + 15 * 60 * 1000 + 1;
      fixture.storage.removeItem.mockImplementation(() => {
        throw new Error('private-storage-error');
      });
      mountTraceViewer(document, fixture.options);
      expect(document.getElementById('trace-report')).toBeNull();
      expect(document.body.textContent).toContain('reload once');
      expect(document.body.textContent).not.toContain('private');
      expect(fixture.storage.removeItem).toHaveBeenCalledWith(TRACE_REPORT_STORAGE_KEY);
      expect(request).not.toHaveBeenCalled();
    }
  );
  it('renders all sections with provenance and keeps setup facts out of publisher facts', () => {
    const fixture = setup();
    mountTraceViewer(document, fixture.options);
    const report = document.getElementById('trace-report');
    expect(report?.textContent).toContain('Browser-carried, unverified diagnostic data');
    for (const title of [
      'What happened',
      'Ad slots',
      'Server auctions',
      'Publisher request',
      'Cookie health',
      'Coverage and ambiguity',
      'Export',
    ])
      expect(report?.textContent).toContain(title);
    expect(report?.textContent).toContain('Browser observed');
    expect(report?.textContent).toContain('Correlation unknown');
    expect(report?.textContent).toContain('0 × 0');
    expect(report?.textContent).toContain('Unavailable');
    expect(report?.textContent).not.toContain('setup-only.example.com');
    expect(document.body.textContent).toContain('setup-only.example.com');
    expect(report?.querySelector('style, [style], script, iframe, img')).toBeNull();
    expect(request).not.toHaveBeenCalled();
  });
  it('displays all four ambiguous cookie rows as unavailable for reliable inspection', () => {
    const report = reportFixture();
    for (const name of ['ts_ec', 'ts_eids', 'ts_tester', 'diagnostics_session'])
      Object.assign(report.request_context.cookies, {
        [name]: { source: 'request', state: 'unavailable', detail: 'runtime_header_ambiguous' },
      });
    const fixture = setup(report);
    mountTraceViewer(document, fixture.options);
    const text = document.getElementById('trace-report-cookies')?.textContent ?? '';
    expect(text.match(/could not be reliably inspected/g)).toHaveLength(4);
    expect(text).not.toContain('invalid UTF');
    expect(text).not.toContain('Not present');
  });
  it('leads with what happened, links slots to auctions and keeps bars CSP-safe', () => {
    const report = joinedReport();
    const cycle = report.gpt_diagnostics.slots[0].requests[0];
    cycle.requestedAtMs = 4175.300000000047;
    cycle.durations.requestToResponseMs = 550.2999999998137;
    const fixture = setup(report);
    mountTraceViewer(document, fixture.options);
    const summary = document.getElementById('trace-report-summary');
    expect(summary?.querySelector('.trace-headline')?.textContent).toBe(
      '1 of 1 ad slot filled · 0 Trusted Server bids'
    );
    expect(summary?.textContent).toContain('The server auction completed.');
    expect(summary?.textContent).toContain('No bids were returned to Trusted Server.');
    const card = document.getElementById('trace-slot-1');
    expect(card?.querySelector('h3')?.textContent).toBe('Slot 1 · 300 × 250');
    expect(card?.querySelector('.trace-chip')?.textContent).toBe('Filled · backfill');
    expect(card?.querySelector('.trace-link')?.textContent).toBe(
      'Linked to auction 1 (Initial-page server auction (SSAT)) · server slot 1 · No candidate'
    );
    const bar = card?.querySelector('.trace-timing');
    expect(bar?.getAttribute('role')).toBe('img');
    expect(bar?.getAttribute('aria-label')).toContain('response 550.3 ms');
    for (const segment of Array.from(bar?.children ?? []))
      expect(segment.className).toMatch(/trace-weight-([1-9]|1\d|20)$/);
    const attention = document.querySelector<HTMLAnchorElement>('#trace-report-attention a');
    expect(attention?.getAttribute('href')).toBe('#trace-slot-1');
    const text = document.getElementById('trace-report')?.textContent ?? '';
    expect(text).toContain('4175.3 ms');
    expect(text).not.toMatch(/\d\.\d{2,} ms/);
    expect(text).not.toMatch(/\bwon\b|\bwinner was\b/i);
    expect(document.querySelector('#trace-report [style]')).toBeNull();
    expect(document.getElementById('trace-report-request')?.textContent).toContain(
      '2026-10-05 09:15:30 UTC'
    );
    const divider = Array.from(document.querySelectorAll('#trace-report *')).find(
      (node) => node.textContent === 'More detail'
    );
    expect(divider?.tagName).toBe('P');
    expect(
      Array.from(document.querySelectorAll('#trace-report h2'), (heading) => heading.textContent)
    ).not.toContain('More detail');
    const visible = Array.from(document.querySelectorAll('#trace-report dl > dd')).filter(
      (value) => !value.closest('details.trace-unavailable')
    );
    expect(visible.map((value) => value.textContent)).not.toContain('Unavailable');
    const folded = document.querySelector('#trace-report details.trace-unavailable');
    expect(folded?.querySelector('summary')?.textContent).toMatch(/^\d+ fields? unavailable$/);
    expect(folded?.hasAttribute('open')).toBe(false);
    for (const id of ['trace-report-request', 'trace-report-cookies', 'trace-report-coverage'])
      expect(document.getElementById(id)?.hasAttribute('open')).toBe(false);
  });
  it('renders exact joined server evidence and retains plain refresh path labels without inferring a winner', () => {
    const report = joinedReport();
    report.gpt_diagnostics.slots[0].requests[0].requestPath = 'prebid_refresh';
    const fixture = setup(report);
    mountTraceViewer(document, fixture.options);
    const text = document.getElementById('trace-report')?.textContent;
    expect(text).toContain('Initial-page server auction (SSAT)');
    expect(text).toContain(
      'Produced by Trusted Server; copied through an untrusted browser snapshot'
    );
    expect(text).toContain('Auction-wide provider status; per-slot no-bid reason unavailable');
    expect(text).toContain('Unavailable in v1');
    expect(text).toContain('Browser refresh observed; winner not determined');
    expect(text).toContain('Trusted Server creative rendered');
    expect(text).toContain('must not be summed as unique bids');
    expect(text).toContain('Correlation record 1');
    expect(text).toContain('Browser observed correlation');
    expect(text).not.toContain('client-side auction won');
  });
  it('uses one immutable public model for explicit Copy, Share and Download and retains it after failures', async () => {
    const fixture = setup();
    fixture.copy.mockResolvedValue({ status: 'failed' });
    fixture.share.mockResolvedValue({ status: 'unsupported' });
    mountTraceViewer(document, fixture.options);
    expect(fixture.copy).not.toHaveBeenCalled();
    expect(fixture.share).not.toHaveBeenCalled();
    expect(fixture.download).not.toHaveBeenCalled();
    button('Copy').click();
    await vi.waitFor(() =>
      expect(document.getElementById('trace-export-status')?.textContent).toContain(
        'Copy could not'
      )
    );
    button('Share').click();
    await vi.waitFor(() =>
      expect(document.getElementById('trace-export-status')?.textContent).toContain(
        'File sharing is unavailable'
      )
    );
    button('Download').click();
    const copied = fixture.copy.mock.calls[0];
    const shared = fixture.share.mock.calls[0];
    const downloaded = fixture.download.mock.calls[0];
    expect(copied).toEqual(shared);
    expect(copied).toEqual(downloaded);
    expect(formatTraceReport(...copied)).toBe(JSON.stringify(reportFixture(), null, 2));
    expect(Object.isFrozen(copied[0])).toBe(true);
    expect(document.getElementById('trace-report')).not.toBeNull();
    expect(document.body.textContent).toContain('selected app receives this JSON');
  });
  it('requires confirmation before any cleanup mutation', () => {
    const fixture = setup();
    fixture.options.confirm.mockReturnValue(false);
    mountTraceViewer(document, fixture.options);
    button('Clear report and end tracing').click();
    expect(fixture.storage.removeItem).not.toHaveBeenCalled();
    expect(request).not.toHaveBeenCalled();
    expect(document.getElementById('trace-report')).not.toBeNull();
  });
  it('deletes the local report independently without confirmation or any server request', () => {
    const fixture = setup();
    mountTraceViewer(document, fixture.options);
    button('Delete local report').click();
    expect(fixture.options.confirm).not.toHaveBeenCalled();
    expect(fixture.storage.removeItem).toHaveBeenCalledWith(TRACE_REPORT_STORAGE_KEY);
    expect(request).not.toHaveBeenCalled();
    expect(document.getElementById('trace-report')).toBeNull();
    expect(document.body.textContent).toContain('Local report deleted');
  });
  it('moves focus to the local status when deletion hides its focused control', () => {
    const fixture = setup();
    mountTraceViewer(document, fixture.options);
    const remove = button('Delete local report');
    remove.focus();
    remove.click();
    expect(remove.hidden).toBe(true);
    expect(document.activeElement).toBe(document.getElementById('trace-cleanup-local-status'));
  });
  it('keeps another cleanup control focused during local deletion', () => {
    const fixture = setup();
    mountTraceViewer(document, fixture.options);
    const clear = button('Clear report and end tracing');
    clear.focus();
    button('Delete local report').click();
    expect(document.activeElement).toBe(clear);
  });
  for (const local of ['deleted', 'failed'] as const)
    for (const mutation of ['requested', 'failed'] as const)
      for (const observation of ['inactive', 'active', 'failed'] as const) {
        it(`separates local ${local}, end ${mutation}, observed ${observation}`, async () => {
          const fixture = setup();
          if (local === 'failed')
            fixture.storage.removeItem.mockImplementation(() => {
              throw new Error('private-deletion-error');
            });
          request.mockResolvedValueOnce(
            new Response('{}', { status: mutation === 'requested' ? 200 : 500 })
          );
          if (observation === 'failed')
            request.mockRejectedValueOnce(new Error('private-state-error'));
          else
            request.mockResolvedValueOnce(
              new Response(JSON.stringify({ observed_active: observation === 'active' }))
            );
          mountTraceViewer(document, fixture.options);
          button('Clear report and end tracing').click();
          await vi.waitFor(() => expect(request).toHaveBeenCalledTimes(2));
          await vi.waitFor(() =>
            expect(document.getElementById('trace-cleanup-server-status')?.textContent).toContain(
              mutation === 'requested' && observation === 'inactive'
                ? 'Tracing is off'
                : 'unconfirmed'
            )
          );
          expect(document.getElementById('trace-report') === null).toBe(local === 'deleted');
          expect(document.getElementById('trace-cleanup-local-status')?.textContent).toContain(
            local === 'deleted' ? 'Local report deleted' : 'deletion failed'
          );
          expect(request.mock.calls.map(([path]) => path)).toEqual([
            '/_ts/trace/end',
            '/_ts/trace/state',
          ]);
          expect(document.body.textContent).not.toContain('private-');
          expect(document.body.textContent).not.toContain('cookie absent');
          if (local === 'failed') expect(button('Delete local report').disabled).toBe(false);
          if (!(mutation === 'requested' && observation === 'inactive'))
            expect(button('Retry end tracing').hidden).toBe(false);
        });
      }
  it('ignores removed controls and late async results after viewer destruction', async () => {
    const fixture = setup();
    let resolveCopy: ((result: Awaited<ReturnType<typeof copyTraceReport>>) => void) | undefined;
    fixture.copy.mockImplementation(
      () =>
        new Promise((resolve) => {
          resolveCopy = resolve;
        })
    );
    const viewer = mountTraceViewer(document, fixture.options);
    const copy = button('Copy');
    const enable = button('Enable tracing');
    copy.click();
    viewer.destroy();
    copy.click();
    enable.click();
    resolveCopy?.({ status: 'copied' });
    await Promise.resolve();
    expect(fixture.copy).toHaveBeenCalledTimes(1);
    expect(request).not.toHaveBeenCalled();
    expect(document.getElementById('trace-export-status')?.textContent).not.toBe('Copied JSON.');
  });
  it('allows separate local deletion and server retry after both earlier cleanup steps fail', async () => {
    const fixture = setup();
    fixture.storage.removeItem.mockImplementationOnce(() => {
      throw new Error('private-storage-error');
    });
    request.mockResolvedValueOnce(new Response('{}', { status: 500 }));
    request.mockResolvedValueOnce(new Response('{"observed_active":true}'));
    mountTraceViewer(document, fixture.options);
    button('Clear report and end tracing').click();
    await vi.waitFor(() => expect(button('Retry end tracing').hidden).toBe(false));
    button('Delete local report').click();
    expect(document.getElementById('trace-report')).toBeNull();
    request.mockResolvedValueOnce(new Response('{}'));
    request.mockResolvedValueOnce(new Response('{"observed_active":false}'));
    const retry = button('Retry end tracing');
    retry.focus();
    retry.click();
    await vi.waitFor(() =>
      expect(document.getElementById('trace-cleanup-server-status')?.textContent).toContain(
        'Tracing is off'
      )
    );
    expect(fixture.options.confirm).toHaveBeenCalledTimes(1);
    expect(fixture.storage.removeItem).toHaveBeenCalledTimes(2);
    expect(request).toHaveBeenCalledTimes(4);
    expect(document.activeElement).toBe(document.getElementById('trace-cleanup-server-status'));
    expect(document.getElementById('trace-cleanup-local-status')?.textContent).toContain(
      'Local report deleted'
    );
  });
  it('does not move focus back from another control when a pending retry succeeds', async () => {
    const fixture = setup();
    request.mockResolvedValueOnce(new Response('{}', { status: 500 }));
    request.mockResolvedValueOnce(new Response('{"observed_active":true}'));
    mountTraceViewer(document, fixture.options);
    button('Clear report and end tracing').click();
    await vi.waitFor(() => expect(button('Retry end tracing').hidden).toBe(false));
    request.mockResolvedValueOnce(new Response('{}'));
    let resolveState: ((response: Response) => void) | undefined;
    request.mockImplementationOnce(
      () =>
        new Promise((resolve) => {
          resolveState = resolve;
        })
    );
    const retry = button('Retry end tracing');
    retry.focus();
    retry.click();
    await vi.waitFor(() => expect(request).toHaveBeenCalledTimes(4));
    const retained = button('Return to previous page');
    retained.focus();
    resolveState?.(new Response('{"observed_active":false}'));
    await vi.waitFor(() => expect(retry.hidden).toBe(true));
    expect(document.activeElement).toBe(retained);
  });
  it('still deletes the report and attempts state verification when both network steps are offline', async () => {
    const fixture = setup();
    request.mockRejectedValueOnce(new Error('private-post-error'));
    request.mockRejectedValueOnce(new Error('private-get-error'));
    mountTraceViewer(document, fixture.options);
    button('Clear report and end tracing').click();
    await vi.waitFor(() =>
      expect(document.getElementById('trace-cleanup-server-status')?.textContent).toContain(
        'may remain active'
      )
    );
    expect(request).toHaveBeenCalledTimes(2);
    expect(document.getElementById('trace-report')).toBeNull();
    expect(document.body.textContent).not.toContain('private-');
  });
  it('keeps local deletion available while the independent end request is pending', async () => {
    const fixture = setup();
    fixture.storage.removeItem.mockImplementationOnce(() => {
      throw new Error('private-storage-error');
    });
    let resolvePost: ((value: Response) => void) | undefined;
    request.mockImplementationOnce(
      () =>
        new Promise((resolve) => {
          resolvePost = resolve;
        })
    );
    request.mockResolvedValueOnce(new Response('{"observed_active":false}'));
    mountTraceViewer(document, fixture.options);
    button('Clear report and end tracing').click();
    expect(button('Delete local report').disabled).toBe(false);
    button('Delete local report').click();
    expect(document.getElementById('trace-report')).toBeNull();
    resolvePost?.(new Response('{}'));
    await vi.waitFor(() =>
      expect(document.getElementById('trace-cleanup-server-status')?.textContent).toContain(
        'Tracing is off'
      )
    );
    expect(request).toHaveBeenCalledTimes(2);
  });
  it('renders an allowed hostile-looking network value as text without executable content or inline style', () => {
    const report = reportFixture();
    Object.assign(report.request_context.network, { tls_cipher: '<img src=x onerror=alert(1)>' });
    const fixture = setup(report);
    mountTraceViewer(document, fixture.options);
    expect(document.getElementById('trace-report')?.textContent).toContain(
      '<img src=x onerror=alert(1)>'
    );
    expect(document.querySelector('img, iframe, script, [style]')).toBeNull();
    expect(request).not.toHaveBeenCalled();
  });
});
