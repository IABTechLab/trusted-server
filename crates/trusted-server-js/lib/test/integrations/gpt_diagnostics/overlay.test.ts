import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { log } from '../../../src/core/log';
import type { GptDiagnosticsBinding } from '../../../src/core/types';
import type { GptDiagnosticsBindingView } from '../../../src/integrations/gpt_diagnostics/binding';
import {
  GPT_DIAGNOSTICS_HOST_ID,
  GptDiagnosticsOverlay,
} from '../../../src/integrations/gpt_diagnostics/overlay';
import { GptDiagnosticsStore } from '../../../src/integrations/gpt_diagnostics/store';

class FakeBindings {
  private readonly listeners = new Set<() => void>();
  private readonly values = new Map<number, GptDiagnosticsBindingView>();

  get(runtimeSlotNumber: number): GptDiagnosticsBindingView {
    return (
      this.values.get(runtimeSlotNumber) ?? {
        binding: { status: 'unbound', reason: 'missing_element' },
        visible: false,
      }
    );
  }

  set(
    runtimeSlotNumber: number,
    binding: GptDiagnosticsBinding,
    element?: HTMLElement,
    visible = false
  ): void {
    this.values.set(runtimeSlotNumber, { binding, element, visible });
    for (const listener of this.listeners) listener();
  }

  subscribe(listener: () => void): () => void {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  }
}

function slot(id: string, adUnitPath = `/example/site/${id}`) {
  return {
    getSlotElementId: () => id,
    getAdUnitPath: () => adUnitPath,
  };
}

function button(root: ShadowRoot, label: string): HTMLButtonElement {
  const match = Array.from(root.querySelectorAll('button')).find(
    (candidate) => candidate.textContent === label
  );
  if (!(match instanceof HTMLButtonElement)) throw new Error(`Missing ${label} button`);
  return match;
}

function runNextFrame(frames: Array<() => void>): void {
  const frame = frames.shift();
  if (!frame) throw new Error('Missing scheduled frame');
  frame();
}

beforeEach(() => {
  document.body.replaceChildren();
  vi.spyOn(document, 'readyState', 'get').mockReturnValue('complete');
});

afterEach(() => {
  vi.restoreAllMocks();
  document.body.replaceChildren();
});

describe('GptDiagnosticsOverlay', () => {
  it('waits for document completion and two animation frames before mounting', () => {
    const frames: Array<() => void> = [];
    const store = new GptDiagnosticsStore({ schedule: (callback) => callback() });
    store.recordSlotRequested(slot('early-slot'));
    let root: ShadowRoot | undefined;
    const overlay = new GptDiagnosticsOverlay(store, new FakeBindings(), {
      scheduleFrame: (callback) => frames.push(callback),
      onShadowRoot: (createdRoot) => {
        root = createdRoot;
      },
    });

    expect(document.getElementById(GPT_DIAGNOSTICS_HOST_ID)).toBeNull();
    runNextFrame(frames);
    expect(document.getElementById(GPT_DIAGNOSTICS_HOST_ID)).toBeNull();
    runNextFrame(frames);

    const host = document.getElementById(GPT_DIAGNOSTICS_HOST_ID);
    expect(host).not.toBeNull();
    expect(host!.shadowRoot).toBeNull();
    expect(root?.textContent).toContain('early-slot');
    overlay.destroy();
  });

  it('renders lifecycle facts, coverage, history, controls, and scoped styles', () => {
    const frames: Array<() => void> = [];
    let now = 10;
    const store = new GptDiagnosticsStore({
      now: () => now,
      schedule: (callback) => callback(),
    });
    const bindings = new FakeBindings();
    const filledSlot = slot('filled-slot');
    const element = document.createElement('div');
    element.id = 'filled-slot';
    document.body.append(element);
    store.recordSlotRequested(filledSlot);
    now = 20;
    store.recordSlotResponseReceived(filledSlot);
    now = 25;
    store.recordSlotRenderEnded(filledSlot, {
      isEmpty: false,
      size: [300, 250],
      isBackfill: true,
    });
    now = 30;
    store.recordSlotOnload(filledSlot);
    now = 35;
    store.recordImpressionViewable(filledSlot);
    store.recordSlotVisibilityChanged(filledSlot, 60);
    now = 40;
    store.recordSlotRequested(filledSlot);
    now = 50;
    store.recordSlotResponseReceived(filledSlot);
    now = 51;
    store.recordSlotRenderEnded(filledSlot, { isEmpty: true });
    const pendingSlot = slot('pending-slot');
    store.recordSlotRequested(pendingSlot);
    const issueSlot = slot('issue-slot');
    store.recordSlotRenderEnded(issueSlot, { isEmpty: false });
    const incompleteSlot = slot('incomplete-slot');
    store.recordSlotRequested(incompleteSlot);
    store.recordSlotRenderEnded(incompleteSlot, { isEmpty: false });
    bindings.set(1, { status: 'bound' }, element, true);
    bindings.set(2, { status: 'unbound', reason: 'missing_element' });
    bindings.set(3, { status: 'ambiguous', reason: 'duplicate_dom_id' });
    bindings.set(4, { status: 'unbound', reason: 'missing_element' });
    const exportSnapshot = vi.fn();
    let root: ShadowRoot | undefined;
    const overlay = new GptDiagnosticsOverlay(store, bindings, {
      scheduleFrame: (callback) => frames.push(callback),
      onExport: exportSnapshot,
      onShadowRoot: (createdRoot) => {
        root = createdRoot;
      },
    });
    runNextFrame(frames);
    runNextFrame(frames);

    expect(root).toBeDefined();
    expect(root!.querySelector('style')?.textContent).toContain('.tsgd-panel');
    expect(root!.textContent).toContain('GPT observed');
    expect(root!.textContent).toContain('callback issues');
    expect(root!.textContent).toContain('filled-slot');
    expect(root!.textContent).toContain('/example/site/filled-slot');
    expect(root!.textContent).toContain('Empty');
    expect(root!.textContent).toContain('Previous requests (1)');
    expect(root!.textContent).toContain('Rendered size 300×250');
    expect(root!.textContent).toContain('Backfill yes');
    expect(root!.textContent).toContain('Loaded');
    expect(root!.textContent).toContain('Viewable');
    expect(root!.textContent).toContain('Request → response 10 ms');
    expect(root!.textContent).toContain('GPT visibility 60%');
    expect(root!.textContent).toContain('Requesting');
    expect(root!.textContent).toContain('Ambiguous binding');
    expect(root!.textContent).toContain('Incomplete sequence');

    button(root!, 'Export JSON').click();
    expect(exportSnapshot).toHaveBeenCalledTimes(1);

    button(root!, 'Collapse').click();
    expect(root!.textContent).not.toContain('filled-slot');
    expect(button(root!, 'Expand').getAttribute('aria-expanded')).toBe('false');
    button(root!, 'Expand').click();

    const filter = root!.querySelector('select');
    expect(filter).toBeInstanceOf(HTMLSelectElement);
    filter!.value = 'visible';
    filter!.dispatchEvent(new Event('change'));
    expect(root!.textContent).toContain('filled-slot');
    expect(root!.textContent).not.toContain('pending-slot');

    filter!.value = 'filled';
    filter!.dispatchEvent(new Event('change'));
    expect(root!.textContent).toContain('incomplete-slot');
    expect(root!.textContent).not.toContain('pending-slot');

    filter!.value = 'empty';
    filter!.dispatchEvent(new Event('change'));
    expect(root!.textContent).toContain('filled-slot');
    expect(root!.textContent).not.toContain('incomplete-slot');

    filter!.value = 'pending';
    filter!.dispatchEvent(new Event('change'));
    expect(root!.textContent).toContain('pending-slot');
    expect(root!.textContent).toContain('incomplete-slot');

    filter!.value = 'unbound';
    filter!.dispatchEvent(new Event('change'));
    expect(root!.textContent).toContain('pending-slot');
    expect(root!.textContent).toContain('issue-slot');
    expect(element.getAttributeNames()).toEqual(['id']);
    expect(element.className).toBe('');
    expect(element.getAttribute('style')).toBeNull();
    overlay.destroy();
  });

  it('does not remove a publisher element that collides with the host ID', async () => {
    const frames: Array<() => void> = [];
    const publisherElement = document.createElement('div');
    publisherElement.id = GPT_DIAGNOSTICS_HOST_ID;
    publisherElement.textContent = 'Publisher element';
    document.body.append(publisherElement);
    const warn = vi.spyOn(log, 'warn');
    const overlay = new GptDiagnosticsOverlay(new GptDiagnosticsStore(), new FakeBindings(), {
      scheduleFrame: (callback) => frames.push(callback),
    });
    runNextFrame(frames);
    runNextFrame(frames);

    expect(document.getElementById(GPT_DIAGNOSTICS_HOST_ID)).toBe(publisherElement);
    expect(publisherElement.textContent).toBe('Publisher element');
    expect(publisherElement.shadowRoot).toBeNull();
    expect(warn).toHaveBeenCalledTimes(1);
    overlay.show();
    expect(warn).toHaveBeenCalledTimes(1);

    publisherElement.remove();
    await Promise.resolve();
    runNextFrame(frames);
    expect(document.getElementById(GPT_DIAGNOSTICS_HOST_ID)).not.toBeNull();
    overlay.destroy();
  });

  it('labels completed renders with unknown fill and preserves live panel state', () => {
    const frames: Array<() => void> = [];
    const store = new GptDiagnosticsStore({ schedule: (callback) => callback() });
    const diagnosticSlot = slot('unknown-render');
    store.recordSlotRequested(diagnosticSlot);
    store.recordSlotResponseReceived(diagnosticSlot);
    store.recordSlotRenderEnded(diagnosticSlot, {});
    store.recordSlotRequested(diagnosticSlot);
    let root: ShadowRoot | undefined;
    const overlay = new GptDiagnosticsOverlay(store, new FakeBindings(), {
      scheduleFrame: (callback) => frames.push(callback),
      onShadowRoot: (createdRoot) => {
        root = createdRoot;
      },
    });
    runNextFrame(frames);
    runNextFrame(frames);

    const content = root!.querySelector<HTMLElement>('.tsgd-content')!;
    const history = root!.querySelector<HTMLDetailsElement>('details')!;
    history.open = true;
    content.scrollTop = 42;
    store.recordSlotResponseReceived(diagnosticSlot);
    runNextFrame(frames);

    expect(root!.textContent).toContain('Rendered (fill unknown)');
    expect(root!.querySelector<HTMLDetailsElement>('details')?.open).toBe(true);
    expect(root!.querySelector<HTMLElement>('.tsgd-content')?.scrollTop).toBe(42);
    overlay.destroy();
  });

  it('remounts after external removal but not after hide or Close', async () => {
    const frames: Array<() => void> = [];
    const store = new GptDiagnosticsStore();
    let root: ShadowRoot | undefined;
    const overlay = new GptDiagnosticsOverlay(store, new FakeBindings(), {
      scheduleFrame: (callback) => frames.push(callback),
      onShadowRoot: (createdRoot) => {
        root = createdRoot;
      },
    });
    runNextFrame(frames);
    runNextFrame(frames);
    const initialHost = document.getElementById(GPT_DIAGNOSTICS_HOST_ID)!;

    initialHost.remove();
    await Promise.resolve();
    runNextFrame(frames);
    const remountedHost = document.getElementById(GPT_DIAGNOSTICS_HOST_ID);
    expect(remountedHost).not.toBeNull();
    expect(remountedHost).not.toBe(initialHost);
    expect(document.querySelectorAll(`#${GPT_DIAGNOSTICS_HOST_ID}`)).toHaveLength(1);

    overlay.hide();
    await Promise.resolve();
    expect(document.getElementById(GPT_DIAGNOSTICS_HOST_ID)).toBeNull();
    expect(frames).toEqual([]);

    store.recordSlotRequested(slot('hidden-period-slot'));
    overlay.show();
    expect(document.getElementById(GPT_DIAGNOSTICS_HOST_ID)).not.toBeNull();
    expect(root!.textContent).toContain('hidden-period-slot');

    button(root!, 'Close').click();
    await Promise.resolve();
    expect(document.getElementById(GPT_DIAGNOSTICS_HOST_ID)).toBeNull();
    overlay.show();
    expect(document.querySelectorAll(`#${GPT_DIAGNOSTICS_HOST_ID}`)).toHaveLength(1);
    overlay.destroy();
  });
});
