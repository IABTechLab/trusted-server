import { beforeEach, afterEach, describe, expect, it, vi } from 'vitest';

import { mountTraceSetup } from '../../src/trace/setup';

const setupContext = {
  schema_version: 1,
  captured_at: '2026-10-05T10:15:30Z',
  network: { masked_client_ip: '192.0.2.0/24' },
  cookies: Object.fromEntries(
    ['ts_ec', 'ts_eids', 'ts_tester', 'diagnostics_session'].map((name) => [
      name,
      { source: 'request', state: 'unavailable', detail: 'runtime_header_ambiguous' },
    ])
  ),
};

function fixture(active = false) {
  document.body.innerHTML = `
    <p id="trace-session-state" data-observed-active="${active}"></p>
    <p id="trace-status" role="status" aria-live="polite"></p>
    <pre id="trace-request-context"></pre>
    <dl id="trace-network-facts"></dl><dl id="trace-cookie-facts"></dl>
    <button id="trace-enable" type="button">Enable tracing</button>
    <button id="trace-end" type="button">End tracing</button>
    <button id="trace-back" type="button">Return to previous page</button>`;
  document.querySelector('#trace-request-context')!.textContent = JSON.stringify(setupContext);
}

async function flush() {
  await vi.waitFor(() =>
    expect(document.querySelector<HTMLButtonElement>('#trace-enable')?.disabled).toBe(false)
  );
}

describe('mobile trace setup', () => {
  const request = vi.fn<typeof fetch>();
  beforeEach(() => {
    request.mockReset();
    vi.stubGlobal('fetch', request);
    fixture();
  });
  afterEach(() => {
    vi.unstubAllGlobals();
    vi.restoreAllMocks();
    document.body.replaceChildren();
  });

  it('renders setup facts without automatically activating or claiming history', () => {
    mountTraceSetup();
    expect(request).not.toHaveBeenCalled();
    expect(document.querySelector('#trace-session-state')?.textContent).toBe(
      'Tracing is off — no valid diagnostics session observed'
    );
    expect(document.querySelector('#trace-network-facts')?.textContent).toContain('192.0.2.0/24');
    expect(document.querySelector('#trace-cookie-facts')?.textContent).toContain(
      'runtime-visible cookies could not be reliably inspected'
    );
  });

  it('renders existing server observation without a mutation request', () => {
    fixture(true);
    mountTraceSetup();
    expect(document.querySelector('#trace-session-state')?.textContent).toBe(
      'Tracing is on — cookie observed by server'
    );
    expect(request).not.toHaveBeenCalled();
  });

  it.each([null, '', 'yes', '1', 'False'])(
    'does not invent an inactive observation from invalid setup state %s',
    (attribute) => {
      const state = document.querySelector('#trace-session-state')!;
      if (attribute === null) state.removeAttribute('data-observed-active');
      else state.setAttribute('data-observed-active', attribute);
      mountTraceSetup();
      expect(state.textContent).toBe('Tracing state unconfirmed');
      expect(request).not.toHaveBeenCalled();
    }
  );

  it('confirms activation only after state verification and gives reload instructions', async () => {
    request.mockResolvedValueOnce(new Response('{}'));
    request.mockResolvedValueOnce(new Response('{"observed_active":true}'));
    mountTraceSetup();
    document.querySelector<HTMLButtonElement>('#trace-enable')!.click();
    expect(document.querySelector<HTMLButtonElement>('#trace-end')?.disabled).toBe(true);
    await flush();
    expect(document.querySelector('#trace-session-state')?.textContent).toContain('Tracing is on');
    expect(document.querySelector('#trace-status')?.textContent).toContain('reload once');
    expect(document.querySelector('#trace-status')?.textContent).toContain('View trace results');
  });

  it('keeps ambiguous activation unconfirmed and allows explicit retry', async () => {
    request.mockResolvedValueOnce(new Response('{}'));
    request.mockResolvedValueOnce(new Response('{"observed_active":false}'));
    mountTraceSetup();
    document.querySelector<HTMLButtonElement>('#trace-enable')!.click();
    await flush();
    expect(document.querySelector('#trace-status')?.textContent).toContain(
      'Activation unconfirmed'
    );
    expect(document.querySelector('#trace-session-state')?.textContent).toContain(
      'no valid diagnostics session observed'
    );
    request.mockResolvedValueOnce(new Response('{}'));
    request.mockResolvedValueOnce(new Response('{"observed_active":true}'));
    document.querySelector<HTMLButtonElement>('#trace-enable')!.click();
    await flush();
    expect(document.querySelector('#trace-session-state')?.textContent).toContain('Tracing is on');
  });

  it('allows end with ambiguous cookies and describes observation without claiming absence', async () => {
    request.mockResolvedValueOnce(new Response('{}'));
    request.mockResolvedValueOnce(new Response('{"observed_active":false}'));
    mountTraceSetup();
    document.querySelector<HTMLButtonElement>('#trace-end')!.click();
    await flush();
    expect(document.querySelector('#trace-status')?.textContent).toContain(
      'no valid diagnostics session observed'
    );
    expect(document.body.textContent).not.toContain('cookie absent');
  });

  it('reports failed observation and restores action controls', async () => {
    request.mockResolvedValueOnce(new Response('{}'));
    request.mockRejectedValueOnce(new Error('secret-error-sentinel'));
    mountTraceSetup();
    document.querySelector<HTMLButtonElement>('#trace-end')!.click();
    await flush();
    expect(document.querySelector('#trace-status')?.textContent).toContain(
      'Deactivation unconfirmed'
    );
    expect(document.querySelector('#trace-session-state')?.textContent).toBe(
      'Tracing state unconfirmed'
    );
    expect(document.body.textContent).not.toContain('secret-error-sentinel');
  });

  it('fails closed on malformed setup facts without displaying a forbidden value', () => {
    document.querySelector('#trace-request-context')!.textContent =
      '{"secret":"raw-cookie-sentinel"}';
    mountTraceSetup();
    expect(document.querySelector('#trace-network-facts')?.textContent).toContain('Unavailable');
    expect(document.querySelector('#trace-cookie-facts')?.textContent).not.toContain(
      'raw-cookie-sentinel'
    );
  });

  it('uses history only on explicit back and supplies a same-host same-tab fallback', () => {
    const back = vi.spyOn(window.history, 'back').mockImplementation(() => {});
    vi.spyOn(window.history, 'length', 'get').mockReturnValue(1);
    mountTraceSetup();
    document.querySelector<HTMLButtonElement>('#trace-back')!.click();
    expect(back).not.toHaveBeenCalled();
    expect(document.querySelector('#trace-status')?.textContent).toContain(
      'exact same hostname and in this same tab'
    );
    vi.spyOn(window.history, 'length', 'get').mockReturnValue(2);
    document.querySelector<HTMLButtonElement>('#trace-back')!.click();
    expect(back).toHaveBeenCalledOnce();
  });
});
