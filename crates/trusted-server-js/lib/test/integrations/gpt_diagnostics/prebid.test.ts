import { describe, expect, it, vi } from 'vitest';

import { GptDiagnosticsPrebidRecorder } from '../../../src/integrations/gpt_diagnostics/prebid';

function fixture() {
  let now = 0;
  const target = { performance: { now: () => now }, tsjs: { navGeneration: 1 } };
  const recorder = { recordPrebidAuction: vi.fn(), recordPrebidWin: vi.fn() };
  const diagnostics = new GptDiagnosticsPrebidRecorder(target, recorder);
  let listener: ((bid: unknown) => void) | undefined;
  const prebid = {
    onEvent: vi.fn((_event: 'bidWon', callback: (bid: unknown) => void) => {
      listener = callback;
    }),
  };
  const slot = {
    getTargeting: (key: string) => (key === 'hb_bidder' ? ['client'] : ['1.20']),
  };
  return {
    diagnostics,
    recorder,
    prebid,
    slot,
    target,
    setNow: (value: number) => {
      now = value;
    },
    win: (auctionId: string, adUnitCode = 'slot') =>
      listener?.({
        auctionId,
        adUnitCode,
        adserverTargeting: { hb_bidder: 'client', hb_pb: '1.20' },
      }),
  };
}

describe('optional Prebid diagnostics recorder', () => {
  it('evicts the oldest attempt after 128 entries and consumes wins once', () => {
    const f = fixture();
    for (let index = 0; index < 129; index += 1) {
      f.diagnostics.recordCompletedPrebidAuction(f.prebid, `auction-${index}`, [f.slot], ['slot']);
    }
    expect(f.prebid.onEvent).toHaveBeenCalledOnce();
    f.win('auction-0');
    expect(f.recorder.recordPrebidWin).not.toHaveBeenCalled();
    f.win('auction-1');
    f.win('auction-1');
    expect(f.recorder.recordPrebidWin).toHaveBeenCalledExactlyOnceWith(f.slot, 'auction-1', {
      bidder: 'client',
      priceBucket: '1.20',
    });
  });

  it('accepts the 30-second boundary, rejects expired and destroyed attempts', () => {
    const f = fixture();
    f.diagnostics.recordCompletedPrebidAuction(f.prebid, 'boundary', [f.slot], ['slot']);
    f.diagnostics.recordCompletedPrebidAuction(f.prebid, 'expired', [f.slot], ['slot']);
    f.setNow(30_000);
    f.win('boundary');
    expect(f.recorder.recordPrebidWin).toHaveBeenCalledOnce();
    f.setNow(30_001);
    f.win('expired');
    expect(f.recorder.recordPrebidWin).toHaveBeenCalledOnce();
    f.diagnostics.recordCompletedPrebidAuction(f.prebid, 'destroyed', [f.slot], ['slot']);
    f.diagnostics.destroy();
    f.win('destroyed');
    f.diagnostics.recordCompletedPrebidAuction(f.prebid, 'later', [f.slot], ['slot']);
    expect(f.recorder.recordPrebidWin).toHaveBeenCalledOnce();
    expect(f.recorder.recordPrebidAuction).toHaveBeenCalledTimes(3);
  });

  it.each(['available', 'absent', 'throws'] as const)(
    'releases its recorder on destroy and reinstalls with offEvent %s',
    (unsubscribe) => {
      const f = fixture();
      const publisher = vi.fn();
      const listeners = new Set<(bid: unknown) => void>([publisher]);
      const offEvent = vi.fn((_event: 'bidWon', listener: (bid: unknown) => void) => {
        if (unsubscribe === 'throws') throw new Error('unsubscribe failed');
        listeners.delete(listener);
      });
      const prebid = {
        onEvent: (_event: 'bidWon', listener: (bid: unknown) => void) => listeners.add(listener),
        offEvent: unsubscribe === 'absent' ? undefined : offEvent,
      };
      f.diagnostics.recordCompletedPrebidAuction(prebid, 'old-auction', [f.slot], ['slot']);
      const oldListener = [...listeners].find((listener) => listener !== publisher)!;
      f.diagnostics.destroy();
      f.diagnostics.destroy();
      expect(Reflect.get(f.diagnostics, 'recorder')).toBeUndefined();
      expect(listeners.has(publisher)).toBe(true);
      expect(listeners.has(oldListener)).toBe(unsubscribe !== 'available');
      if (unsubscribe !== 'absent') {
        expect(offEvent).toHaveBeenCalledExactlyOnceWith('bidWon', oldListener);
      }
      oldListener({
        auctionId: 'old-auction',
        adUnitCode: 'slot',
        adserverTargeting: { hb_bidder: 'client', hb_pb: '1.20' },
      });
      expect(f.recorder.recordPrebidWin).not.toHaveBeenCalled();

      const currentRecorder = { recordPrebidAuction: vi.fn(), recordPrebidWin: vi.fn() };
      const current = new GptDiagnosticsPrebidRecorder(f.target, currentRecorder);
      current.recordCompletedPrebidAuction(prebid, 'new-auction', [f.slot], ['slot']);
      for (const listener of listeners) {
        listener({
          auctionId: 'new-auction',
          adUnitCode: 'slot',
          adserverTargeting: { hb_bidder: 'client', hb_pb: '1.20' },
        });
      }
      expect(publisher).toHaveBeenCalledOnce();
      expect(f.recorder.recordPrebidWin).not.toHaveBeenCalled();
      expect(currentRecorder.recordPrebidWin).toHaveBeenCalledExactlyOnceWith(
        f.slot,
        'new-auction',
        {
          bidder: 'client',
          priceBucket: '1.20',
        }
      );
      expect(listeners.size).toBe(unsubscribe === 'available' ? 2 : 3);
      current.destroy();
    }
  );

  it('continues cleanup on other exact instances after one offEvent throws', () => {
    const f = fixture();
    const failed = {
      onEvent: vi.fn(),
      offEvent: vi.fn(() => {
        throw new Error('failed');
      }),
    };
    const working = { onEvent: vi.fn(), offEvent: vi.fn() };
    f.diagnostics.recordCompletedPrebidAuction(failed, 'failed', [f.slot], ['slot']);
    f.diagnostics.recordCompletedPrebidAuction(working, 'working', [f.slot], ['slot']);
    expect(() => f.diagnostics.destroy()).not.toThrow();
    expect(failed.offEvent).toHaveBeenCalledExactlyOnceWith(
      'bidWon',
      failed.onEvent.mock.calls[0][1]
    );
    expect(working.offEvent).toHaveBeenCalledExactlyOnceWith(
      'bidWon',
      working.onEvent.mock.calls[0][1]
    );
  });

  it('retries when the API arrives later and subscribes to the passed instance only', () => {
    const f = fixture();
    const delayed: { onEvent?: typeof f.prebid.onEvent } = {};
    f.diagnostics.recordCompletedPrebidAuction(delayed, 'stub', [f.slot], ['slot']);
    delayed.onEvent = f.prebid.onEvent;
    f.diagnostics.recordCompletedPrebidAuction(delayed, 'ready', [f.slot], ['slot']);
    f.diagnostics.recordCompletedPrebidAuction(delayed, 'ready-again', [f.slot], ['slot']);
    expect(delayed.onEvent).toHaveBeenCalledOnce();
    f.win('ready');
    expect(f.recorder.recordPrebidWin).toHaveBeenCalledOnce();
  });

  it("does not let a different subscribed instance consume another instance's attempt", () => {
    const f = fixture();
    let otherWin: ((bid: unknown) => void) | undefined;
    const other = {
      onEvent: (_event: 'bidWon', listener: (bid: unknown) => void) => {
        otherWin = listener;
      },
    };
    f.diagnostics.recordCompletedPrebidAuction(f.prebid, 'shared-id', [f.slot], ['slot']);
    f.diagnostics.recordCompletedPrebidAuction(other, 'shared-id', [f.slot], ['slot']);
    f.win('shared-id');
    expect(f.recorder.recordPrebidWin).not.toHaveBeenCalled();
    otherWin?.({
      auctionId: 'shared-id',
      adUnitCode: 'slot',
      adserverTargeting: { hb_bidder: 'client', hb_pb: '1.20' },
    });
    expect(f.recorder.recordPrebidWin).toHaveBeenCalledOnce();
  });

  it.each([undefined, '', ' padded ', 'é'.repeat(129)])(
    'rejects invalid auction ID %s',
    (auctionId) => {
      const f = fixture();
      f.diagnostics.recordCompletedPrebidAuction(f.prebid, auctionId, [f.slot], ['slot']);
      expect(f.recorder.recordPrebidAuction).not.toHaveBeenCalled();
      expect(f.prebid.onEvent).not.toHaveBeenCalled();
    }
  );

  it.each([
    { bidder: ['é'.repeat(65)], bucket: ['1.20'] },
    { bidder: ['one', 'two'], bucket: ['1.20'] },
    { bidder: ['client'], bucket: ['1'.repeat(65)] },
    { bidder: ['client'], bucket: ['NaN'] },
  ])('omits invalid targeting candidates without dropping the attempt', ({ bidder, bucket }) => {
    const f = fixture();
    const slot = { getTargeting: (key: string) => (key === 'hb_bidder' ? bidder : bucket) };
    f.diagnostics.recordCompletedPrebidAuction(f.prebid, 'valid-id', [slot], ['slot']);
    expect(f.recorder.recordPrebidAuction).toHaveBeenCalledWith(slot, 'valid-id', undefined);
    f.win('valid-id');
    expect(f.recorder.recordPrebidWin).toHaveBeenCalledOnce();
  });
});
