import { describe, expect, it, vi } from 'vitest';

import { ConsentSync } from '../../../src/integrations/sourcepoint/consent_sync';

const ok = { status: 204, body: null } as Response;

function snapshot(gppString: string | null) {
  return { gppString, applicableSections: gppString ? [7] : [] };
}

describe('Sourcepoint server consent reconciliation', () => {
  it('sends the exact decision with same-origin credentials and keepalive', () => {
    const fetch = vi.fn<typeof window.fetch>().mockResolvedValue(ok);
    const sync = new ConsentSync(fetch);
    sync.publish(snapshot('synthetic-current-gpp'));
    expect(fetch).toHaveBeenCalledWith('/_ts/consent', {
      method: 'POST',
      credentials: 'same-origin',
      redirect: 'error',
      cache: 'no-store',
      keepalive: true,
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(snapshot('synthetic-current-gpp')),
    });
  });

  it('deduplicates repeated snapshots but preserves a withdrawal between grants', () => {
    const fetch = vi.fn<typeof window.fetch>().mockResolvedValue(ok);
    const sync = new ConsentSync(fetch);
    sync.publish(snapshot('synthetic-allow'));
    sync.publish(snapshot('synthetic-allow'));
    sync.publish(snapshot('synthetic-deny'));
    sync.publish(snapshot('synthetic-allow'));
    expect(fetch).toHaveBeenCalledTimes(3);
    expect(fetch.mock.calls.map(([, init]) => JSON.parse(init!.body as string).gppString)).toEqual([
      'synthetic-allow',
      'synthetic-deny',
      'synthetic-allow',
    ]);
  });

  it('does not wait for an earlier response before dispatching a withdrawal', () => {
    const fetch = vi.fn<typeof window.fetch>().mockImplementation(() => new Promise(() => {}));
    const sync = new ConsentSync(fetch);
    sync.publish(snapshot('synthetic-allow'));
    sync.publish(snapshot('synthetic-deny'));
    expect(fetch).toHaveBeenCalledTimes(2);
  });

  it('permits a later event to retry an unconfirmed update without a retry timer', async () => {
    const fetch = vi
      .fn<typeof window.fetch>()
      .mockResolvedValueOnce({ status: 503, body: null } as Response)
      .mockResolvedValue(ok);
    const sync = new ConsentSync(fetch);
    sync.publish(snapshot('synthetic-deny'));
    await Promise.resolve();
    sync.publish(snapshot('synthetic-deny'));
    expect(fetch).toHaveBeenCalledTimes(2);
  });

  it.each([200, 202])(
    'does not treat a generic HTTP %i as reconciliation success',
    async (status) => {
      const fetch = vi
        .fn<typeof window.fetch>()
        .mockResolvedValueOnce({ status, body: null } as Response)
        .mockResolvedValue(ok);
      const sync = new ConsentSync(fetch);
      sync.publish(snapshot('synthetic-deny'));
      await Promise.resolve();
      sync.publish(snapshot('synthetic-deny'));
      expect(fetch).toHaveBeenCalledTimes(2);
    }
  );

  it('contains a synchronous fetch failure and permits a later retry', () => {
    const fetch = vi
      .fn<typeof window.fetch>()
      .mockImplementationOnce(() => {
        throw new Error('synthetic failure');
      })
      .mockResolvedValue(ok);
    const sync = new ConsentSync(fetch);
    expect(() => sync.publish(snapshot('synthetic-deny'))).not.toThrow();
    sync.publish(snapshot('synthetic-deny'));
    expect(fetch).toHaveBeenCalledTimes(2);
  });

  it('does not cancel an in-flight keepalive update when disposed', () => {
    const fetch = vi.fn<typeof window.fetch>().mockImplementation(() => new Promise(() => {}));
    const sync = new ConsentSync(fetch);
    sync.publish(snapshot('synthetic-deny'));
    sync.stop();
    sync.publish(snapshot('synthetic-allow'));
    expect(fetch).toHaveBeenCalledTimes(1);
    expect(fetch.mock.calls[0][1]?.signal).toBeUndefined();
  });
});
