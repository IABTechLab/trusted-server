import { describe, expect, it, vi } from 'vitest';

import type { TsjsApi } from '../../src/core/types';
import { createTraceCollector } from '../../src/trace/collector';
import type { downloadTraceReport } from '../../src/trace/export';
import { createTraceHandoff } from '../../src/trace/handoff';

import { gptSourceFixture, reportFixture, TRACE_NOW, TRACE_ORIGIN } from './fixtures';

function setup(active: unknown = true) {
  const order: string[] = [];
  const snapshot = vi.fn(() => {
    order.push('snapshot');
    return gptSourceFixture();
  });
  const collector = createTraceCollector();
  const target = {
    __tsjs_trace_active: active,
    __tsjs_trace_request_context: reportFixture().request_context,
    tsjs: {
      traceEvidence: collector,
      gptDiagnostics: { snapshot },
    } as unknown as TsjsApi,
    location: {
      origin: TRACE_ORIGIN,
      assign: vi.fn((path: string) => {
        order.push(`navigate:${path}`);
      }),
    },
    sessionStorage: {
      getItem: vi.fn(() => null),
      setItem: vi.fn((_key: string, _value: string) => {
        order.push('store');
      }),
      removeItem: vi.fn(),
    },
  };
  const states: unknown[] = [];
  const download = vi.fn<typeof downloadTraceReport>(() => ({ status: 'downloaded' }));
  const options = {
    target,
    now: () => TRACE_NOW,
    onChange: (state: unknown): void => {
      states.push(state);
    },
    download,
  };
  return { order, snapshot, collector, target, states, download, options };
}
function handoff(options: ReturnType<typeof setup>['options']) {
  const result = createTraceHandoff(options);
  if (typeof result !== 'object' || result === null) throw new Error('should create handoff');
  return result;
}
describe('explicit same-tab trace handoff', () => {
  it.each([undefined, false, 'true', 1])(
    'creates no action or storage access for nonliteral gate %s',
    (active) => {
      const fixture = setup();
      fixture.target.__tsjs_trace_active = active;
      expect(createTraceHandoff(fixture.options)).toBeUndefined();
      expect(fixture.snapshot).not.toHaveBeenCalled();
      expect(fixture.target.sessionStorage.getItem).not.toHaveBeenCalled();
      expect(fixture.target.sessionStorage.setItem).not.toHaveBeenCalled();
    }
  );
  it('captures only on tap then stores a validated wrapper before same-tab navigation', () => {
    const fixture = setup();
    const action = handoff(fixture.options);
    expect(fixture.order).toEqual([]);
    expect(fixture.target.sessionStorage.getItem).not.toHaveBeenCalled();
    action.view();
    expect(fixture.order).toEqual(['snapshot', 'store', 'navigate:/_ts/trace']);
    const wrapper = JSON.parse(fixture.target.sessionStorage.setItem.mock.calls[0][1]);
    expect(wrapper.report.request_context).toEqual(fixture.target.__tsjs_trace_request_context);
    expect(wrapper.report.gpt_diagnostics.page.pathname).toBe('/[redacted]');
    expect(wrapper.stored_at_ms).toBe(TRACE_NOW);
  });
  it('preserves a valid immutable combined report for explicit direct download after storage failure', () => {
    const fixture = setup();
    fixture.target.sessionStorage.setItem.mockImplementation(() => {
      throw new Error('private-quota-error');
    });
    const action = handoff(fixture.options);
    action.view();
    expect(fixture.target.location.assign).not.toHaveBeenCalled();
    expect(fixture.download).not.toHaveBeenCalled();
    expect(fixture.states[fixture.states.length - 1]).toMatchObject({
      kind: 'storage_unavailable',
      downloadAvailable: true,
    });
    fixture.target.__tsjs_trace_request_context.network = {
      private_value: 'changed-after-capture',
    } as object;
    action.download();
    const report = fixture.download.mock.calls[0][0];
    expect(report).toMatchObject({ request_context: reportFixture().request_context });
    expect(Object.isFrozen(report)).toBe(true);
    expect(fixture.download.mock.calls[0].slice(1)).toEqual([TRACE_ORIGIN, TRACE_NOW]);
  });
  it.each([
    'missing-context',
    'invalid-context',
    'bad-gpt-version',
    'gpt-throws',
    'collector-overflow',
    'clock-invalid',
  ])('never writes, navigates or offers a combined artifact after %s', (failure) => {
    const fixture = setup();
    if (failure === 'missing-context')
      Object.assign(fixture.target, {
        __tsjs_trace_request_context: undefined,
      });
    if (failure === 'invalid-context')
      Object.assign(fixture.target, {
        __tsjs_trace_request_context: { private_value: 'secret' },
      });
    if (failure === 'bad-gpt-version')
      fixture.snapshot.mockReturnValue({
        ...gptSourceFixture(),
        version: 2,
      } as never);
    if (failure === 'gpt-throws')
      fixture.snapshot.mockImplementation(() => {
        throw new Error('private-source-error');
      });
    if (failure === 'collector-overflow')
      Object.assign(fixture.target.tsjs, {
        traceEvidence: {
          snapshot: () => ({
            ok: false,
            reason: 'omission_counter_overflow',
          }),
        },
      });
    if (failure === 'clock-invalid') fixture.options.now = () => Number.NaN;
    const action = handoff(fixture.options);
    action.view();
    action.download();
    expect(fixture.target.sessionStorage.setItem).not.toHaveBeenCalled();
    expect(fixture.target.location.assign).not.toHaveBeenCalled();
    expect(fixture.download).not.toHaveBeenCalled();
    expect(JSON.stringify(fixture.states)).not.toContain('private');
    expect(fixture.states[fixture.states.length - 1]).toMatchObject({
      kind: 'capture_failed',
      downloadAvailable: false,
    });
  });
  it('remains on the publisher page if its navigation attempt fails after storage succeeds', () => {
    const fixture = setup();
    fixture.target.location.assign.mockImplementation(() => {
      throw new Error('private-navigation-error');
    });
    const action = handoff(fixture.options);
    action.view();
    expect(fixture.target.sessionStorage.setItem).toHaveBeenCalledTimes(1);
    expect(fixture.states[fixture.states.length - 1]).toMatchObject({
      kind: 'navigation_unavailable',
      downloadAvailable: true,
    });
  });
  it.each(['capturing', 'snapshot', 'storage'])(
    'stops capture when destroyed from %s callback',
    (phase) => {
      const fixture = setup();
      if (phase === 'capturing') fixture.options.onChange = () => action.destroy();
      if (phase === 'snapshot')
        fixture.snapshot.mockImplementation(() => {
          action.destroy();
          return gptSourceFixture();
        });
      if (phase === 'storage')
        fixture.target.sessionStorage.setItem.mockImplementation(() => action.destroy());
      const action = handoff(fixture.options);
      action.view();
      action.download();
      expect(fixture.target.location.assign).not.toHaveBeenCalled();
      expect(fixture.download).not.toHaveBeenCalled();
      if (phase === 'capturing') expect(fixture.snapshot).not.toHaveBeenCalled();
      if (phase !== 'storage') expect(fixture.target.sessionStorage.setItem).not.toHaveBeenCalled();
    }
  );
  it('destroys retained recovery data and ignores later retry callbacks', () => {
    const fixture = setup();
    fixture.target.sessionStorage.setItem.mockImplementation(() => {
      throw new Error('private-quota-error');
    });
    const action = handoff(fixture.options);
    action.view();
    action.destroy();
    action.download();
    action.view();
    expect(fixture.snapshot).toHaveBeenCalledTimes(1);
    expect(fixture.download).not.toHaveBeenCalled();
  });
});
