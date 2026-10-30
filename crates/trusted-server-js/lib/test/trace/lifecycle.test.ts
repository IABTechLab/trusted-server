import { beforeEach, afterEach, describe, expect, it, vi } from 'vitest';

import { changeTraceSession, endTraceSessionAndObserve } from '../../src/trace/lifecycle';

describe('deliberate trace session changes', () => {
  const request = vi.fn<typeof fetch>();

  beforeEach(() => {
    request.mockReset();
    vi.stubGlobal('fetch', request);
  });

  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it.each(['enable', 'end'] as const)(
    'verifies %s through a separate state GET',
    async (action) => {
      const active = action === 'enable';
      request.mockResolvedValueOnce(new Response('{}', { status: 200 }));
      request.mockResolvedValueOnce(new Response(JSON.stringify({ observed_active: active })));
      const historyLength = window.history.length;
      const result = await changeTraceSession(action);

      expect(request.mock.calls).toEqual([
        [
          `/_ts/trace/${action}`,
          {
            method: 'POST',
            credentials: 'same-origin',
            cache: 'no-store',
            headers: { 'X-TS-Trace-Action': action },
          },
        ],
        ['/_ts/trace/state', { method: 'GET', credentials: 'same-origin', cache: 'no-store' }],
      ]);
      expect(result).toEqual({
        mutation: 'requested',
        observation: active ? 'active' : 'inactive',
        confirmed: true,
      });
      expect(window.history.length).toBe(historyLength);
    }
  );

  it('does not confirm activation when the browser fails to return a valid session', async () => {
    request.mockResolvedValueOnce(new Response('{}'));
    request.mockResolvedValueOnce(new Response('{"observed_active":false}'));
    expect(await changeTraceSession('enable')).toEqual({
      mutation: 'requested',
      observation: 'inactive',
      confirmed: false,
    });
  });

  it('does not confirm deactivation when a valid session is still observed', async () => {
    request.mockResolvedValueOnce(new Response('{}'));
    request.mockResolvedValueOnce(new Response('{"observed_active":true}'));
    expect(await changeTraceSession('end')).toEqual({
      mutation: 'requested',
      observation: 'active',
      confirmed: false,
    });
  });

  it.each([
    '{}',
    '{"observed_active":"false"}',
    '{"observed_active":false,"extra":true}',
    '[]',
    'null',
    '{invalid-json',
  ])(
    'rejects an invalid state response without discarding the mutation result: %s',
    async (body) => {
      request.mockResolvedValueOnce(new Response('{}'));
      request.mockResolvedValueOnce(new Response(body));
      expect(await changeTraceSession('enable')).toEqual({
        mutation: 'requested',
        observation: 'failed',
        confirmed: false,
      });
    }
  );

  it('retains requested mutation when the verification request fails', async () => {
    request.mockResolvedValueOnce(new Response('{}'));
    request.mockRejectedValueOnce(new TypeError('offline'));
    expect(await changeTraceSession('end')).toEqual({
      mutation: 'requested',
      observation: 'failed',
      confirmed: false,
    });
  });

  it('rejects a non-successful state response', async () => {
    request.mockResolvedValueOnce(new Response('{}'));
    request.mockResolvedValueOnce(new Response('{"observed_active":false}', { status: 500 }));
    expect((await changeTraceSession('end')).observation).toBe('failed');
  });

  it.each([403, 404, 413, 500])(
    'keeps a rejected mutation (%s) separate from state observation',
    async (status) => {
      request.mockResolvedValueOnce(new Response('{}', { status }));
      expect(await changeTraceSession('enable')).toEqual({
        mutation: 'failed',
        observation: 'not_attempted',
        confirmed: false,
      });
      expect(request).toHaveBeenCalledTimes(1);
    }
  );

  it('permits an explicit retry after a failed mutation', async () => {
    request.mockRejectedValueOnce(new TypeError('offline'));
    expect((await changeTraceSession('enable')).mutation).toBe('failed');
    request.mockResolvedValueOnce(new Response('{}'));
    request.mockResolvedValueOnce(new Response('{"observed_active":true}'));
    expect((await changeTraceSession('enable')).confirmed).toBe(true);
    expect(request).toHaveBeenCalledTimes(3);
  });

  it.each(['rejected', 'offline'] as const)(
    'observes state independently after an end POST is %s',
    async (failure) => {
      if (failure === 'offline') request.mockRejectedValueOnce(new Error('private-network-error'));
      else request.mockResolvedValueOnce(new Response('{}', { status: 500 }));
      request.mockResolvedValueOnce(new Response('{"observed_active":false}'));
      expect(await endTraceSessionAndObserve()).toEqual({
        mutation: 'failed',
        observation: 'inactive',
        confirmed: false,
      });
      expect(request.mock.calls.map(([path]) => path)).toEqual([
        '/_ts/trace/end',
        '/_ts/trace/state',
      ]);
    }
  );

  it('keeps both failed end mutation and failed independent observation bounded', async () => {
    request.mockRejectedValueOnce(new Error('private-post-error'));
    request.mockRejectedValueOnce(new Error('private-state-error'));
    expect(await endTraceSessionAndObserve()).toEqual({
      mutation: 'failed',
      observation: 'failed',
      confirmed: false,
    });
    expect(request).toHaveBeenCalledTimes(2);
  });
});
