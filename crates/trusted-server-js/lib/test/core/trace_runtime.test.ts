import { describe, expect, it, vi } from 'vitest';

import {
  createRenderTrace,
  DiagnosticsSubscriberLimitError,
  TRACE_BADGE_CLASS,
  TRACE_PANEL_ID,
  type RenderTraceGptFactV1,
} from '../../src/core/trace';

function harness() {
  const tasks: Array<() => void> = [];
  const owner = createRenderTrace({
    scheduler: {
      set: (callback) => {
        tasks.push(callback);
        return callback;
      },
      clear: (handle) => {
        const index = tasks.indexOf(handle as () => void);
        if (index >= 0) tasks.splice(index, 1);
      },
    },
  });
  return {
    owner,
    tasks,
    drain: (): void => {
      while (tasks.length > 0) tasks.shift()?.();
    },
  };
}

describe('render trace diagnostics runtime', () => {
  it('exposes one exact frozen read-only public surface with copied snapshots', () => {
    const { owner } = harness();
    const target = window as unknown as { tsjs?: Record<string, unknown> };
    const existingApi = (target.tsjs = {});
    const event = vi.fn();
    window.addEventListener('tsjs:adRendered', event);
    const record = owner.record({ slotId: 'slot-a', path: 'auction', rendered: true });

    expect(Reflect.ownKeys(owner.diagnostics).sort()).toEqual(['current', 'history', 'subscribe']);
    expect(Object.isFrozen(owner.diagnostics)).toBe(true);
    const current = owner.diagnostics.current();
    const history = owner.diagnostics.history();
    expect(Object.isFrozen(current)).toBe(true);
    expect(Object.isFrozen(history)).toBe(true);
    expect(Object.isFrozen(current['slot-a'])).toBe(true);
    expect(current['slot-a']).toEqual(record);
    expect(current['slot-a']).not.toBe(record);
    expect(history[0]).toEqual(record);
    expect(history[0]).not.toBe(record);
    expect(target.tsjs).toBe(existingApi);
    expect(target.tsjs).toEqual({});
    expect(event).not.toHaveBeenCalled();
    window.removeEventListener('tsjs:adRendered', event);
    delete target.tsjs;
  });

  it('commits before one asynchronous frozen public delivery', () => {
    const { owner, tasks, drain } = harness();
    const listener = vi.fn();
    owner.diagnostics.subscribe(listener);

    const record = owner.record({ slotId: 'slot-a', path: 'ssat', rendered: true });

    expect(owner.diagnostics.current()['slot-a']).toEqual(record);
    expect(listener).not.toHaveBeenCalled();
    expect(tasks).toHaveLength(1);
    drain();
    expect(listener).toHaveBeenCalledTimes(1);
    const delivered = listener.mock.calls[0]?.[0];
    expect(delivered).toEqual(record);
    expect(delivered).not.toBe(record);
    expect(Object.isFrozen(delivered)).toBe(true);
  });

  it('enforces the 32-subscriber cap after callable validation and reuses capacity', () => {
    const { owner } = harness();
    const releases = Array.from({ length: 32 }, () => owner.diagnostics.subscribe(() => undefined));

    expect(() => owner.diagnostics.subscribe(null as never)).toThrow(TypeError);
    expect(() => owner.diagnostics.subscribe(() => undefined)).toThrowError(
      expect.objectContaining({ code: 'subscriber_capacity', surface: 'renderTrace' })
    );
    expect(() => owner.diagnostics.subscribe(() => undefined)).toThrow(
      DiagnosticsSubscriberLimitError
    );
    releases[0]?.();
    releases[0]?.();
    expect(owner.diagnostics.subscribe(() => undefined)).toBeTypeOf('function');
  });

  it('captures membership per commit and suppresses unsubscribe before delivery', () => {
    const { owner, drain } = harness();
    const first = vi.fn();
    const second = vi.fn();
    const releaseFirst = owner.diagnostics.subscribe(first);
    owner.record({ slotId: 'slot-a', path: 'auction', rendered: true });
    releaseFirst();
    owner.diagnostics.subscribe(second);
    drain();
    expect(first).not.toHaveBeenCalled();
    expect(second).not.toHaveBeenCalled();

    owner.record({ slotId: 'slot-b', path: 'auction', rendered: true });
    drain();
    expect(second).toHaveBeenCalledTimes(1);
  });

  it('coalesces pending same-impression enrichment without changing FIFO order', () => {
    const { owner, drain, tasks } = harness();
    const received: Array<{ seq: number; injected?: boolean }> = [];
    owner.diagnostics.subscribe((record) => received.push(record));
    const first = owner.record({
      slotId: 'slot-a',
      path: 'ssat',
      rendered: true,
      injected: false,
    });
    const second = owner.record({ slotId: 'slot-b', path: 'auction', rendered: false });
    owner.enrich(first!, { injected: true, servedFrom: 'pbs-cache' });

    expect(tasks).toHaveLength(1);
    drain();
    expect(received.map(({ seq }) => seq)).toEqual([first!.seq, second!.seq]);
    expect(received[0]).toEqual(expect.objectContaining({ injected: true }));
  });

  it('bounds current state and history and prunes navigation-owned slots', () => {
    const { owner } = harness();
    for (let index = 0; index < 256; index += 1) {
      expect(
        owner.record({ slotId: `slot-${index}`, path: 'auction', rendered: true })
      ).toBeDefined();
    }
    owner.record({ slotId: 'slot-over-capacity', path: 'auction', rendered: true });
    expect(Object.keys(owner.diagnostics.current())).toHaveLength(256);
    owner.prune('slot-0');
    expect(owner.diagnostics.current()).not.toHaveProperty('slot-0');
    owner.record({ slotId: 'slot-after-prune', path: 'auction', rendered: true });

    for (let index = 0; index < 10; index += 1) {
      owner.record({ slotId: 'slot-1', path: 'gam-refresh', rendered: index % 2 === 0 });
    }
    const history = owner.diagnostics.history();
    expect(history).toHaveLength(200);
    expect(history[0]?.seq).toBeGreaterThan(1);
  });

  it('retains a bounded document-lifetime slot count after current-state pruning', () => {
    const { owner } = harness();
    const first = owner.record({ slotId: 'reused-slot', path: 'auction', rendered: true });

    expect(owner.prune('reused-slot', first.seq)).toBe(true);
    const second = owner.record({ slotId: 'reused-slot', path: 'gam-refresh', rendered: false });

    expect(second.count).toBe(2);
  });

  it('retains impression bookkeeping and refuses truth-weakening enrichment', () => {
    const { owner } = harness();
    const record = owner.record({
      slotId: 'slot-a',
      path: 'ssat',
      rendered: true,
      injected: true,
    })!;

    const enriched = owner.enrich(record, {
      rendered: false,
      injected: false,
      visible: true,
      servedFrom: 'pbs-cache',
    })!;

    expect(enriched).toEqual(
      expect.objectContaining({
        at: record.at,
        count: record.count,
        seq: record.seq,
        rendered: true,
        injected: true,
        visible: true,
        servedFrom: 'pbs-cache',
      })
    );
    expect(owner.diagnostics.history()).toHaveLength(1);
  });

  it('records an unattributed GPT request as one GAM-refresh impression', () => {
    const { owner } = harness();
    const token = Object.freeze(Object.create(null) as object);
    const resolve = () =>
      Object.freeze({ slotId: 'publisher-slot', elementId: 'publisher-slot', visible: true });

    owner.observeGptFact(
      Object.freeze({
        kind: 'slotRequested',
        observedAtMs: 1,
        slot: Object.freeze({ token, elementId: 'publisher-slot' }),
      }),
      resolve
    );
    owner.observeGptFact(
      Object.freeze({
        kind: 'slotRenderEnded',
        observedAtMs: 2,
        slot: Object.freeze({ token, elementId: 'publisher-slot' }),
        isEmpty: false,
      }),
      resolve
    );

    expect(owner.diagnostics.current()['publisher-slot']).toEqual(
      expect.objectContaining({
        path: 'gam-refresh',
        rendered: true,
        gamEmpty: false,
        injected: false,
        visible: true,
        servedFrom: 'gam',
      })
    );
    expect(owner.diagnostics.history()).toHaveLength(1);
  });

  it('reconciles a later trusted terminal into the GPT-first impression', () => {
    const { owner, tasks, drain } = harness();
    const listener = vi.fn();
    const token = Object.freeze(Object.create(null) as object);
    const slot = Object.freeze({ token, elementId: 'reverse-slot' });
    const resolve = () =>
      Object.freeze({ slotId: 'reverse-slot', elementId: 'reverse-slot', visible: true });
    owner.diagnostics.subscribe(listener);

    owner.observeGptFact(Object.freeze({ kind: 'slotRequested', observedAtMs: 1, slot }), resolve);
    owner.observeGptFact(
      Object.freeze({ kind: 'slotRenderEnded', observedAtMs: 2, slot, isEmpty: false }),
      resolve
    );
    const provisional = owner.diagnostics.current()['reverse-slot'];
    expect(provisional).toEqual(
      expect.objectContaining({ path: 'gam-refresh', rendered: true, injected: false })
    );

    const terminal = owner.record({
      slotId: 'reverse-slot',
      path: 'ssat',
      rendered: true,
      injected: true,
      bidder: 'trusted-bidder',
      bidId: 'trusted-bid',
      creativeId: 'trusted-creative',
      servedFrom: 'pbs-cache',
    });

    expect(terminal).toEqual(
      expect.objectContaining({
        seq: provisional?.seq,
        count: provisional?.count,
        at: provisional?.at,
        path: 'ssat',
        bidder: 'trusted-bidder',
        bidId: 'trusted-bid',
        creativeId: 'trusted-creative',
        servedFrom: 'pbs-cache',
        rendered: true,
        injected: true,
        gamEmpty: false,
      })
    );
    expect(owner.diagnostics.history()).toEqual([terminal]);
    expect(tasks).toHaveLength(1);
    drain();
    expect(listener).toHaveBeenCalledOnce();
    expect(listener).toHaveBeenCalledWith(
      expect.objectContaining({ seq: terminal.seq, path: 'ssat' })
    );

    owner.observeGptFact(
      Object.freeze({
        kind: 'slotVisibilityChanged',
        observedAtMs: 3,
        slot,
        inViewPercentage: 0,
      }),
      resolve
    );
    expect(owner.diagnostics.current()['reverse-slot']).toEqual(
      expect.objectContaining({ seq: terminal.seq, path: 'ssat', visible: false })
    );
    expect(owner.diagnostics.history()).toHaveLength(1);
  });

  it('enriches only the same GPT impression without weakening TS placement truth', () => {
    const { owner } = harness();
    const token = Object.freeze(Object.create(null) as object);
    const slot = Object.freeze({ token, elementId: 'ts-slot' });
    const resolve = () => Object.freeze({ slotId: 'ts-slot', elementId: 'ts-slot', visible: true });
    owner.record({ slotId: 'ts-slot', path: 'gam-refresh', rendered: false, injected: false });
    owner.observeGptFact(Object.freeze({ kind: 'slotRequested', observedAtMs: 1, slot }), resolve);
    const trusted = owner.record({
      slotId: 'ts-slot',
      path: 'ssat',
      rendered: true,
      injected: true,
      servedFrom: 'pbs-cache',
    });

    owner.observeGptFact(
      Object.freeze({ kind: 'slotRenderEnded', observedAtMs: 2, slot, isEmpty: false }),
      resolve
    );
    owner.observeGptFact(
      Object.freeze({ kind: 'slotRenderEnded', observedAtMs: 3, slot, isEmpty: true }),
      resolve
    );

    expect(owner.diagnostics.history()).toHaveLength(2);
    expect(owner.diagnostics.current()['ts-slot']).toEqual(
      expect.objectContaining({
        seq: trusted.seq,
        path: 'ssat',
        rendered: true,
        injected: true,
        gamEmpty: false,
        servedFrom: 'pbs-cache',
      })
    );
  });

  it('routes all GPT lifecycle facts and scopes visibility to the active physical request', () => {
    const { owner } = harness();
    const firstToken = Object.freeze(Object.create(null) as object);
    const secondToken = Object.freeze(Object.create(null) as object);
    const resolve = () => Object.freeze({ slotId: 'visible-slot', visible: false });
    const fact = (
      kind: RenderTraceGptFactV1['kind'],
      token: object,
      fields: Readonly<Pick<RenderTraceGptFactV1, 'isEmpty' | 'inViewPercentage'>> = Object.freeze(
        {}
      )
    ): Readonly<RenderTraceGptFactV1> =>
      Object.freeze({ kind, observedAtMs: 1, slot: Object.freeze({ token }), ...fields });

    owner.observeGptFact(fact('slotRequested', firstToken), resolve);
    owner.observeGptFact(fact('slotResponseReceived', firstToken), resolve);
    owner.observeGptFact(fact('slotRenderEnded', firstToken, { isEmpty: false }), resolve);
    owner.observeGptFact(fact('slotOnload', firstToken), resolve);
    owner.observeGptFact(fact('impressionViewable', firstToken), resolve);
    expect(owner.diagnostics.current()['visible-slot']?.visible).toBe(true);
    owner.observeGptFact(
      fact('slotVisibilityChanged', firstToken, { inViewPercentage: 0 }),
      resolve
    );
    expect(owner.diagnostics.current()['visible-slot']?.visible).toBe(false);

    owner.observeGptFact(fact('slotRequested', secondToken), resolve);
    owner.observeGptFact(fact('slotRenderEnded', firstToken, { isEmpty: true }), resolve);
    owner.observeGptFact(fact('impressionViewable', firstToken), resolve);
    owner.observeGptFact(fact('impressionViewable', secondToken), resolve);
    expect(
      owner.diagnostics.current()['visible-slot']?.visible,
      'a pre-render callback for a replacement physical request must not enrich the old impression'
    ).toBe(false);
    expect(owner.diagnostics.history()).toHaveLength(1);
  });

  it('drops the oldest of 201 pending records and cancels work on disposal', () => {
    const { owner, tasks, drain } = harness();
    const listener = vi.fn();
    owner.diagnostics.subscribe(listener);
    for (let index = 0; index < 201; index += 1) {
      owner.record({ slotId: 'slot-a', path: 'auction', rendered: true });
    }
    expect(tasks).toHaveLength(1);
    drain();
    expect(listener).toHaveBeenCalledTimes(200);
    expect(listener.mock.calls[0]?.[0].seq).toBe(2);

    owner.record({ slotId: 'slot-a', path: 'auction', rendered: true });
    expect(tasks).toHaveLength(1);
    owner.dispose();
    owner.dispose();
    drain();
    expect(listener).toHaveBeenCalledTimes(200);
    const late = owner.record({ slotId: 'late', path: 'auction', rendered: true });
    expect(Object.isFrozen(late)).toBe(true);
    expect(owner.diagnostics.current()).toEqual({});
    expect(owner.diagnostics.history()).toEqual([]);
    expect(owner.diagnostics.subscribe(() => undefined)).toBeTypeOf('function');
    expect(() => owner.diagnostics.subscribe(null as never)).toThrow(TypeError);
  });

  it('uses the server-resolved boot bit instead of reading the trace cookie', () => {
    document.cookie = 'ts-trace=1; Path=/';
    const disarmedSlot = document.createElement('div');
    disarmedSlot.id = 'disarmed-slot';
    document.body.append(disarmedSlot);
    const disarmed = createRenderTrace({ document, overlayEnabled: false });

    disarmed.record({
      slotId: 'disarmed-slot',
      elementId: 'disarmed-slot',
      path: 'auction',
      rendered: true,
      injected: true,
      visible: true,
    });

    expect(disarmedSlot.getAttribute('data-ts-rendered')).toBe('true');
    expect(disarmedSlot.querySelector(`.${TRACE_BADGE_CLASS}`)).toBeNull();
    expect(document.getElementById(TRACE_PANEL_ID)).toBeNull();
    disarmed.dispose();
    disarmedSlot.remove();
    document.cookie = 'ts-trace=; Max-Age=0; Path=/';

    const armedSlot = document.createElement('div');
    armedSlot.id = 'armed-slot';
    document.body.append(armedSlot);
    const armed = createRenderTrace({ document, overlayEnabled: true });
    armed.record({
      slotId: 'armed-slot',
      elementId: 'armed-slot',
      path: 'ssat',
      rendered: true,
      injected: true,
      visible: true,
    });

    const badge = armedSlot.querySelector(`.${TRACE_BADGE_CLASS}`) as HTMLElement | null;
    expect(badge).not.toBeNull();
    expect(badge?.style.pointerEvents).toBe('none');
    expect(document.getElementById(TRACE_PANEL_ID)).not.toBeNull();
    armed.dispose();
    armedSlot.remove();
  });

  it('removes stale stamps and badges on a later physical impression', () => {
    const slot = document.createElement('div');
    slot.id = 'restamped-slot';
    document.body.append(slot);
    const owner = createRenderTrace({ document, overlayEnabled: true });
    owner.record({
      slotId: 'restamped-slot',
      elementId: 'restamped-slot',
      path: 'ssat',
      rendered: true,
      injected: true,
      visible: true,
      bidder: 'first-bidder',
      admHash: 'first-hash',
    });
    expect(slot.getAttribute('data-ts-bidder')).toBe('first-bidder');
    expect(slot.querySelector(`.${TRACE_BADGE_CLASS}`)).not.toBeNull();

    owner.record({
      slotId: 'restamped-slot',
      elementId: 'restamped-slot',
      path: 'gam-refresh',
      rendered: true,
      injected: true,
      visible: false,
    });

    expect(slot.hasAttribute('data-ts-bidder')).toBe(false);
    expect(slot.hasAttribute('data-ts-adm-hash')).toBe(false);
    expect(slot.querySelector(`.${TRACE_BADGE_CLASS}`)).toBeNull();
    owner.dispose();
    expect(slot.hasAttribute('data-ts-slot-id')).toBe(false);
    slot.remove();
  });

  it('stamps iframe slots without placing UI inside the creative frame', () => {
    const iframe = document.createElement('iframe');
    iframe.id = 'iframe-slot';
    document.body.append(iframe);
    const owner = createRenderTrace({ document, overlayEnabled: true });

    owner.record({
      slotId: 'iframe-slot',
      elementId: 'iframe-slot',
      path: 'ssat',
      rendered: true,
      injected: true,
      visible: true,
    });

    expect(iframe.getAttribute('data-ts-rendered')).toBe('true');
    expect(iframe.querySelector(`.${TRACE_BADGE_CLASS}`)).toBeNull();
    owner.dispose();
    iframe.remove();
  });

  it('does not claim an ok badge before visibility is positively observed', () => {
    const slot = document.createElement('div');
    slot.id = 'unobserved-slot';
    document.body.append(slot);
    const owner = createRenderTrace({ document, overlayEnabled: true });

    owner.record({
      slotId: 'unobserved-slot',
      elementId: 'unobserved-slot',
      path: 'auction',
      rendered: true,
      injected: true,
    });

    expect(slot.querySelector(`.${TRACE_BADGE_CLASS}`)).toBeNull();
    expect(document.getElementById(TRACE_PANEL_ID)?.textContent).toContain('hidden');
    owner.dispose();
    slot.remove();
  });

  it('does not claim or remove a publisher-owned overlay id collision', () => {
    const publisherPanel = document.createElement('div');
    publisherPanel.id = TRACE_PANEL_ID;
    publisherPanel.textContent = 'publisher';
    document.body.append(publisherPanel);
    const owner = createRenderTrace({ document, overlayEnabled: true });

    owner.record({ slotId: 'slot-a', path: 'auction', rendered: true });

    expect(document.getElementById(TRACE_PANEL_ID)).toBe(publisherPanel);
    expect(publisherPanel.textContent).toBe('publisher');
    owner.dispose();
    expect(document.getElementById(TRACE_PANEL_ID)).toBe(publisherPanel);
    publisherPanel.remove();
  });

  it('keeps a bounded newest-first overlay and exports frozen row data', () => {
    const exportRecord = vi.fn();
    const owner = createRenderTrace({ document, overlayEnabled: true, exportRecord });
    for (let index = 1; index <= 201; index += 1) {
      owner.record({ slotId: `slot-${index}`, path: 'auction', rendered: true });
    }

    const panel = document.getElementById(TRACE_PANEL_ID)!;
    const rows = [...panel.querySelectorAll<HTMLElement>('[data-ts-trace-seq]')];
    expect(rows).toHaveLength(200);
    expect(rows[0]?.dataset['tsTraceSeq']).toBe('201');
    expect(rows[rows.length - 1]?.dataset['tsTraceSeq']).toBe('2');
    rows[0]?.click();
    expect(exportRecord).toHaveBeenCalledOnce();
    expect(exportRecord).toHaveBeenCalledWith(
      expect.objectContaining({ slotId: 'slot-201', seq: 201 })
    );
    expect(Object.isFrozen(exportRecord.mock.calls[0]?.[0])).toBe(true);
    owner.dispose();
    expect(document.getElementById(TRACE_PANEL_ID)).toBeNull();
  });

  it('isolates presentation failures after committing diagnostics state', () => {
    const onPresentationError = vi.fn();
    const hostileDocument = {
      getElementById: () => {
        throw new Error('hostile document');
      },
    } as unknown as Document;
    const owner = createRenderTrace({
      document: hostileDocument,
      overlayEnabled: true,
      onPresentationError,
    });

    expect(() => owner.record({ slotId: 'slot-a', path: 'auction', rendered: true })).not.toThrow();
    expect(owner.diagnostics.current()['slot-a']).toEqual(
      expect.objectContaining({ slotId: 'slot-a', rendered: true })
    );
    expect(onPresentationError).toHaveBeenCalledOnce();
  });
});
