import { describe, expect, it, vi } from 'vitest';

import { TcfReader, type TcfReading } from '../../../src/integrations/prebid/tcf_reader';

type Callback = (data: unknown, success: boolean) => void;

function cmp() {
  let nextId = 1;
  const listeners = new Map<number, Callback>();
  const api = vi.fn(
    (command: string, _version: number, callback: Callback, parameter?: unknown) => {
      if (command === 'addEventListener') {
        const id = nextId++;
        listeners.set(id, callback);
      } else if (command === 'removeEventListener') {
        listeners.delete(parameter as number);
        callback(true, true);
      }
    }
  );
  return {
    api,
    listeners,
    emit(data: Record<string, unknown>, success = true) {
      for (const [listenerId, callback] of listeners) callback({ ...data, listenerId }, success);
    },
  };
}

const granted = {
  gdprApplies: true,
  eventStatus: 'tcloaded',
  tcString: 'CPgrant.AA',
  purpose: { consents: { 1: true, 4: true, 7: false }, legitimateInterests: { 2: true } },
  vendor: { consents: { 755: true }, legitimateInterests: {} },
};

function setup() {
  const target = {} as Window & { __tcfapi?: unknown };
  const readings: TcfReading[] = [];
  const reader = new TcfReader(target, (reading) => readings.push(reading));
  return { target, readings, reader };
}

describe('TcfReader', () => {
  it('reads terminal events with their true-valued consents', () => {
    const { target, readings, reader } = setup();
    const { api, emit } = cmp();
    target.__tcfapi = api;
    reader.refresh();

    emit({ ...granted, eventStatus: 'cmpuishown' });
    expect(readings, 'should ignore a CMP still asking').toEqual([]);
    emit(granted);
    emit({ ...granted, eventStatus: 'useractioncomplete', tcString: 'CPnext.AA' });

    expect(api).toHaveBeenCalledWith('addEventListener', 2, expect.any(Function));
    expect(readings).toEqual([
      { gdprApplies: true, tcString: 'CPgrant.AA', granted: ['p1', 'p4', 'pl2', 'v755'] },
      { gdprApplies: true, tcString: 'CPnext.AA', granted: ['p1', 'p4', 'pl2', 'v755'] },
    ]);
  });

  it('treats out-of-scope GDPR as a reading regardless of status', () => {
    const { target, readings, reader } = setup();
    const { api, emit } = cmp();
    target.__tcfapi = api;
    reader.refresh();
    emit({ gdprApplies: false, eventStatus: 'cmpuishown', tcString: '' });
    expect(readings).toEqual([{ gdprApplies: false, tcString: '', granted: [] }]);
  });

  it('ignores refusals and malformed payloads', () => {
    const { target, readings, reader } = setup();
    const { api, emit, listeners } = cmp();
    target.__tcfapi = api;
    reader.refresh();
    emit(granted, false);
    for (const listener of listeners.values()) listener(null, true);
    emit({ ...granted, gdprApplies: 'yes' });
    emit({ ...granted, eventStatus: undefined });
    expect(readings).toEqual([]);
  });

  it('subscribes once per API and moves to a replacement, removing the old listener', () => {
    const { target, readings, reader } = setup();
    const first = cmp();
    target.__tcfapi = first.api;
    reader.refresh();
    first.emit(granted);
    reader.refresh();
    expect(first.api, 'should not resubscribe an unchanged API').toHaveBeenCalledTimes(1);

    const second = cmp();
    target.__tcfapi = second.api;
    reader.refresh();
    expect(first.api).toHaveBeenCalledWith('removeEventListener', 2, expect.any(Function), 1);
    expect(first.listeners.size).toBe(0);
    second.emit({ ...granted, tcString: 'CPsecond.AA' });
    expect(readings.map((reading) => reading.tcString)).toEqual(['CPgrant.AA', 'CPsecond.AA']);
  });

  it('drops late events from a replaced API', () => {
    const { target, readings, reader } = setup();
    let stale: Callback | undefined;
    target.__tcfapi = vi.fn((command: string, _version: number, callback: Callback) => {
      if (command === 'addEventListener') stale = callback;
    });
    reader.refresh();
    target.__tcfapi = cmp().api;
    reader.refresh();
    stale!({ ...granted, listenerId: 9 }, true);
    expect(readings).toEqual([]);
  });

  it('preserves replacement withdrawals when independent CMPs reuse a listener ID', () => {
    const { target, readings, reader } = setup();
    const first = cmp();
    target.__tcfapi = first.api;
    reader.refresh();
    const stale = first.listeners.get(1)!;

    const second = cmp();
    target.__tcfapi = second.api;
    reader.refresh();
    second.emit(granted);
    stale({ ...granted, listenerId: 1 }, true);
    second.emit({
      ...granted,
      tcString: 'CPwithdraw.AA',
      purpose: { consents: { 1: false, 4: false } },
    });

    expect(second.listeners.size, 'the replacement still owns its listener').toBe(1);
    expect(readings.map((reading) => reading.tcString)).toEqual(['CPgrant.AA', 'CPwithdraw.AA']);
    expect(first.listeners.size, 'cleanup reaches only the accepting CMP').toBe(0);
  });

  it('cleans a superseded forwarding stub through its accepting API only', () => {
    const { target, readings, reader } = setup();
    let queued: Callback | undefined;
    const real = cmp();
    const stub = vi.fn(
      (command: string, version: number, callback: Callback, parameter?: unknown) => {
        if (command === 'addEventListener') queued = callback;
        if (command === 'removeEventListener') real.api(command, version, callback, parameter);
      }
    );
    target.__tcfapi = stub;
    reader.refresh();
    target.__tcfapi = real.api;
    reader.refresh();

    // The forwarding stub's transferred listener belongs to the real CMP.
    real.listeners.set(7, queued!);
    queued!({ ...granted, listenerId: 7 }, true);
    queued!({ ...granted, listenerId: 7 }, true);
    const removals = (fn: ReturnType<typeof vi.fn>) =>
      fn.mock.calls.filter(([command]) => command === 'removeEventListener');
    expect(removals(real.api), 'the real CMP receives the removal once').toEqual([
      ['removeEventListener', 2, expect.any(Function), 7],
    ]);
    expect(removals(stub)).toHaveLength(1);
    expect(real.listeners.has(7)).toBe(false);
    expect(real.listeners.has(1), 'the replacement retains its own subscription').toBe(true);
    expect(readings).toEqual([]);
  });

  it('contains a CMP that throws', () => {
    const { target, readings, reader } = setup();
    target.__tcfapi = vi.fn(() => {
      throw new Error('broken CMP');
    });
    expect(() => reader.refresh()).not.toThrow();
    expect(readings).toEqual([]);
  });

  it('does nothing without a CMP', () => {
    const { readings, reader } = setup();
    expect(() => reader.refresh()).not.toThrow();
    expect(readings).toEqual([]);
  });
});
