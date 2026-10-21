import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import {
  copyTraceReport,
  downloadTraceReport,
  formatTraceReport,
  shareTraceReport,
} from '../../src/trace/export';

import { reportFixture, TRACE_NOW, TRACE_ORIGIN } from './fixtures';

describe('equivalent explicit local trace exports', () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });
  afterEach(() => {
    vi.useRealTimers();
    vi.restoreAllMocks();
    vi.unstubAllGlobals();
    document.body.replaceChildren();
  });
  it('formats only the validated identical public report for all export paths', () => {
    const report = reportFixture();
    const json = formatTraceReport(report, TRACE_ORIGIN, TRACE_NOW);
    expect(json).toBe(JSON.stringify(report, null, 2));
    expect(JSON.parse(json!)).toEqual(report);
    expect(json).not.toContain('stored_at_ms');
    expect(JSON.parse(json!).request_context.cookies.diagnostics_session.detail).toBe(
      'runtime_header_ambiguous'
    );
    expect(
      formatTraceReport({ ...report, private_value: 'secret' }, TRACE_ORIGIN, TRACE_NOW)
    ).toBeUndefined();
  });
  it('downloads the same report with deferred independent cleanup for repeated taps', () => {
    const created: Blob[] = [];
    const create = vi.fn((blob: Blob) => {
      created.push(blob);
      return `blob:trace-${created.length}`;
    });
    const revoke = vi.fn();
    vi.stubGlobal(
      'URL',
      class extends URL {
        static createObjectURL = create;
        static revokeObjectURL = revoke;
      }
    );
    const clicks: { href: string; download: string; connected: boolean }[] = [];
    vi.spyOn(HTMLAnchorElement.prototype, 'click').mockImplementation(function (
      this: HTMLAnchorElement
    ) {
      clicks.push({ href: this.href, download: this.download, connected: this.isConnected });
      expect(revoke).not.toHaveBeenCalled();
    });
    expect(downloadTraceReport(reportFixture(), TRACE_ORIGIN, TRACE_NOW)).toEqual({
      status: 'downloaded',
    });
    expect(downloadTraceReport(reportFixture(), TRACE_ORIGIN, TRACE_NOW)).toEqual({
      status: 'downloaded',
    });
    expect(clicks).toEqual([
      { href: 'blob:trace-1', download: 'trusted-server-trace-v1.json', connected: true },
      { href: 'blob:trace-2', download: 'trusted-server-trace-v1.json', connected: true },
    ]);
    expect(document.querySelector('a')).toBeNull();
    expect(created.map((blob) => blob.type)).toEqual(['application/json', 'application/json']);
    vi.advanceTimersByTime(999);
    expect(revoke).not.toHaveBeenCalled();
    vi.advanceTimersByTime(1);
    expect(revoke.mock.calls).toEqual([['blob:trace-1'], ['blob:trace-2']]);
  });
  it('still schedules URL cleanup when the browser download click throws', () => {
    const revoke = vi.fn();
    vi.stubGlobal(
      'URL',
      class extends URL {
        static createObjectURL = vi.fn(() => 'blob:failed-click');
        static revokeObjectURL = revoke;
      }
    );
    vi.spyOn(HTMLAnchorElement.prototype, 'click').mockImplementation(() => {
      throw new Error('private-click-error');
    });
    expect(downloadTraceReport(reportFixture(), TRACE_ORIGIN, TRACE_NOW)).toEqual({
      status: 'failed',
    });
    expect(document.querySelector('a')).toBeNull();
    expect(revoke).not.toHaveBeenCalled();
    vi.advanceTimersByTime(1000);
    expect(revoke).toHaveBeenCalledWith('blob:failed-click');
  });
  it('copies formatted report JSON only when the explicit action runs', async () => {
    const writeText = vi.fn(async () => undefined);
    vi.stubGlobal('navigator', { clipboard: { writeText } });
    expect(writeText).not.toHaveBeenCalled();
    expect(await copyTraceReport(reportFixture(), TRACE_ORIGIN, TRACE_NOW)).toEqual({
      status: 'copied',
    });
    expect(writeText).toHaveBeenCalledWith(JSON.stringify(reportFixture(), null, 2));
    writeText.mockRejectedValueOnce(new Error('private-clipboard-error'));
    expect(await copyTraceReport(reportFixture(), TRACE_ORIGIN, TRACE_NOW)).toEqual({
      status: 'failed',
    });
  });
  it('returns unsupported without attempting clipboard or URL fallback', async () => {
    const fetch = vi.fn();
    vi.stubGlobal('fetch', fetch);
    vi.stubGlobal('navigator', {});
    expect(await copyTraceReport(reportFixture(), TRACE_ORIGIN, TRACE_NOW)).toEqual({
      status: 'unsupported',
    });
    expect(await shareTraceReport(reportFixture(), TRACE_ORIGIN, TRACE_NOW)).toEqual({
      status: 'unsupported',
    });
    expect(fetch).not.toHaveBeenCalled();
  });
  it('shares a JSON File with exactly the formatted report and no URL', async () => {
    let offered: File | undefined;
    const canShare = vi.fn((data: ShareData) => {
      offered = data.files?.[0];
      return true;
    });
    const share = vi.fn(async (_data: ShareData) => undefined);
    vi.stubGlobal('navigator', { canShare, share });
    expect(await shareTraceReport(reportFixture(), TRACE_ORIGIN, TRACE_NOW)).toEqual({
      status: 'shared',
    });
    expect(offered?.name).toBe('trusted-server-trace-v1.json');
    expect(offered?.type).toBe('application/json');
    expect(Object.keys(share.mock.calls[0][0])).toEqual(['files']);
    expect(share.mock.calls[0][0].files?.[0]).toBe(offered);
    expect(offered?.size).toBe(
      new TextEncoder().encode(JSON.stringify(reportFixture(), null, 2)).length
    );
  });
  it('leaves the valid model available after share refusal or rejection', async () => {
    const report = reportFixture();
    const share = vi.fn(async () => undefined);
    vi.stubGlobal('navigator', { canShare: () => false, share });
    expect(await shareTraceReport(report, TRACE_ORIGIN, TRACE_NOW)).toEqual({
      status: 'unsupported',
    });
    expect(share).not.toHaveBeenCalled();
    vi.stubGlobal('navigator', {
      canShare: () => true,
      share: async () => {
        throw new DOMException('User cancelled', 'AbortError');
      },
    });
    expect(await shareTraceReport(report, TRACE_ORIGIN, TRACE_NOW)).toEqual({ status: 'failed' });
    expect(formatTraceReport(report, TRACE_ORIGIN, TRACE_NOW)).toBe(
      JSON.stringify(report, null, 2)
    );
  });
  it('declines every action before browser side effects for an invalid report', async () => {
    const writeText = vi.fn();
    const share = vi.fn();
    const create = vi.fn();
    vi.stubGlobal('navigator', { clipboard: { writeText }, canShare: () => true, share });
    vi.stubGlobal(
      'URL',
      class extends URL {
        static createObjectURL = create;
      }
    );
    const invalid = { ...reportFixture(), private_value: 'secret' };
    expect(downloadTraceReport(invalid, TRACE_ORIGIN, TRACE_NOW)).toEqual({
      status: 'invalid_report',
    });
    expect(await copyTraceReport(invalid, TRACE_ORIGIN, TRACE_NOW)).toEqual({
      status: 'invalid_report',
    });
    expect(await shareTraceReport(invalid, TRACE_ORIGIN, TRACE_NOW)).toEqual({
      status: 'invalid_report',
    });
    expect(writeText).not.toHaveBeenCalled();
    expect(share).not.toHaveBeenCalled();
    expect(create).not.toHaveBeenCalled();
  });
});
