import { describe, expect, it, vi } from 'vitest';

import {
  readTraceReport,
  storeTraceReport,
  deleteTraceReport,
  TRACE_REPORT_STORAGE_KEY,
} from '../../src/trace/storage';
import { validateTraceStoredReport } from '../../src/trace/report-validation';

import { reportFixture, TRACE_ORIGIN, TRACE_NOW } from './fixtures';

function fixture() {
  return { stored_at_ms: TRACE_NOW, report: reportFixture() };
}
function memory(initial: string | null = null) {
  let value = initial;
  return {
    getItem: vi.fn((key: string) => (key === TRACE_REPORT_STORAGE_KEY ? value : null)),
    setItem: vi.fn((key: string, next: string) => {
      if (key === TRACE_REPORT_STORAGE_KEY) value = next;
    }),
    removeItem: vi.fn((key: string) => {
      if (key === TRACE_REPORT_STORAGE_KEY) value = null;
    }),
  };
}
describe('one origin-local validated trace report', () => {
  it('requires a safe integer capture timestamp in the wrapper validator', () => {
    expect(
      validateTraceStoredReport(
        { ...fixture(), stored_at_ms: TRACE_NOW + 0.5 },
        TRACE_ORIGIN,
        TRACE_NOW
      )
    ).toBe(false);
  });
  it('stores only the validated exact wrapper and replaces the previous explicit capture', () => {
    const storage = memory();
    expect(storeTraceReport(fixture(), TRACE_ORIGIN, TRACE_NOW, storage)).toEqual({
      status: 'stored',
    });
    expect(storage.setItem).toHaveBeenCalledWith(
      TRACE_REPORT_STORAGE_KEY,
      JSON.stringify(fixture())
    );
    const next = fixture();
    next.report.request_context.network = {};
    expect(storeTraceReport(next, TRACE_ORIGIN, TRACE_NOW, storage)).toEqual({ status: 'stored' });
    expect(storage.setItem).toHaveBeenCalledTimes(2);
  });
  it('reads an independently owned frozen model without writing it back', () => {
    const storage = memory(JSON.stringify(fixture()));
    const result = readTraceReport(TRACE_ORIGIN, TRACE_NOW, storage);
    expect(result).toEqual({ status: 'ready', value: fixture() });
    expect(storage.setItem).not.toHaveBeenCalled();
    expect(storage.removeItem).not.toHaveBeenCalled();
    if (typeof result !== 'object' || result === null || !('value' in result))
      throw new Error('should read report');
    expect(Object.isFrozen(result.value)).toBe(true);
  });
  it('distinguishes absent data from unavailable storage without writes', () => {
    expect(readTraceReport(TRACE_ORIGIN, TRACE_NOW, memory())).toEqual({ status: 'absent' });
    const storage = memory();
    storage.getItem.mockImplementation(() => {
      throw new Error('private-read-error');
    });
    expect(readTraceReport(TRACE_ORIGIN, TRACE_NOW, storage)).toEqual({ status: 'unavailable' });
    expect(storage.setItem).not.toHaveBeenCalled();
  });
  it.each([
    '{',
    JSON.stringify({ stored_at_ms: TRACE_NOW }),
    JSON.stringify({ ...fixture(), private_value: 'secret' }),
    JSON.stringify({ ...fixture(), stored_at_ms: TRACE_NOW + 0.5 }),
    JSON.stringify({ ...fixture(), stored_at_ms: TRACE_NOW + 60001 }),
    JSON.stringify({ ...fixture(), stored_at_ms: TRACE_NOW - 900001 }),
    JSON.stringify({ ...fixture(), stored_at_ms: -1 }),
  ])('removes and ignores rejected storage %#', (serialized) => {
    const storage = memory(serialized);
    expect(readTraceReport(TRACE_ORIGIN, TRACE_NOW, storage)).toMatchObject({ status: 'rejected' });
    expect(storage.removeItem).toHaveBeenCalledWith(TRACE_REPORT_STORAGE_KEY);
    expect(storage.setItem).not.toHaveBeenCalled();
  });
  it('accepts exactly fifteen minutes and sixty seconds of future clock skew', () => {
    expect(
      readTraceReport(TRACE_ORIGIN, TRACE_NOW + 900000, memory(JSON.stringify(fixture())))
    ).toMatchObject({ status: 'ready' });
    expect(
      readTraceReport(TRACE_ORIGIN, TRACE_NOW - 60000, memory(JSON.stringify(fixture())))
    ).toMatchObject({ status: 'ready' });
    expect(
      readTraceReport(TRACE_ORIGIN, TRACE_NOW - 60001, memory(JSON.stringify(fixture())))
    ).toMatchObject({ status: 'rejected' });
  });
  it('rejects another origin and an unsupported public version with a bounded reason', () => {
    expect(
      readTraceReport('https://other.example.com', TRACE_NOW, memory(JSON.stringify(fixture())))
    ).toMatchObject({ status: 'rejected', reason: 'invalid_report' });
    const value = fixture();
    value.report.schema_version = 2;
    expect(readTraceReport(TRACE_ORIGIN, TRACE_NOW, memory(JSON.stringify(value)))).toMatchObject({
      status: 'rejected',
      reason: 'unsupported_report_version',
    });
  });
  it('ignores rejected entries even when removal throws', () => {
    const storage = memory('{');
    storage.removeItem.mockImplementation(() => {
      throw new Error('private-remove-error');
    });
    expect(readTraceReport(TRACE_ORIGIN, TRACE_NOW, storage)).toMatchObject({ status: 'rejected' });
  });
  it('rejects oversized serialized UTF-8 before parsing and never writes invalid models', () => {
    const storage = memory(' '.repeat(512 * 1024 + 1));
    expect(readTraceReport(TRACE_ORIGIN, TRACE_NOW, storage)).toEqual({
      status: 'rejected',
      reason: 'invalid_report',
    });
    expect(
      storeTraceReport({ ...fixture(), private_value: 'secret' }, TRACE_ORIGIN, TRACE_NOW, storage)
    ).toEqual({ status: 'rejected', reason: 'invalid_report' });
    expect(storage.setItem).not.toHaveBeenCalled();
  });
  it('preserves the report model when writing fails and exposes no exception text', () => {
    const storage = memory();
    storage.setItem.mockImplementation(() => {
      throw new Error('private-write-error');
    });
    const source = fixture();
    expect(storeTraceReport(source, TRACE_ORIGIN, TRACE_NOW, storage)).toEqual({
      status: 'unavailable',
    });
    expect(source).toEqual(fixture());
    expect(storage.removeItem).not.toHaveBeenCalled();
  });
  it('deletes explicitly and reports failures without modifying another storage key', () => {
    const storage = memory(JSON.stringify(fixture()));
    expect(deleteTraceReport(storage)).toEqual({ status: 'deleted' });
    expect(storage.removeItem).toHaveBeenCalledWith(TRACE_REPORT_STORAGE_KEY);
    storage.removeItem.mockImplementation(() => {
      throw new Error('private-remove-error');
    });
    expect(deleteTraceReport(storage)).toEqual({ status: 'unavailable' });
  });
});
