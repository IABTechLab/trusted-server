import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { GptDiagnosticsBindingManager } from '../../../src/integrations/gpt_diagnostics/binding';
import {
  GptDiagnosticsStore,
  type GptDiagnosticsSlotLike,
} from '../../../src/integrations/gpt_diagnostics/store';

const managers: GptDiagnosticsBindingManager[] = [];

function installCssEscape(): void {
  Object.defineProperty(window, 'CSS', {
    configurable: true,
    value: {
      escape: (value: string) => value.replace(/([^a-zA-Z0-9_-])/g, '\\$1'),
    },
  });
}

function fakeSlot(elementId?: string): GptDiagnosticsSlotLike {
  return {
    getSlotElementId: () => elementId ?? '',
    getAdUnitPath: () => '/example/site/banner',
  };
}

function createStore(): GptDiagnosticsStore {
  return new GptDiagnosticsStore({
    now: () => 1,
    schedule: (callback) => callback(),
  });
}

function createManager(
  store: GptDiagnosticsStore,
  scheduleFrame?: (callback: () => void) => () => void
): GptDiagnosticsBindingManager {
  const manager = new GptDiagnosticsBindingManager(store, { scheduleFrame });
  managers.push(manager);
  return manager;
}

function queueFrame(frames: Array<() => void>): (callback: () => void) => () => void {
  return (callback) => {
    frames.push(callback);
    return () => {
      const index = frames.indexOf(callback);
      if (index >= 0) frames.splice(index, 1);
    };
  };
}

function setRectangle(
  element: HTMLElement,
  rectangle: { top: number; left: number; width: number; height: number }
): void {
  const { top, left, width, height } = rectangle;
  vi.spyOn(element, 'getBoundingClientRect').mockReturnValue({
    x: left,
    y: top,
    top,
    left,
    width,
    height,
    right: left + width,
    bottom: top + height,
    toJSON: () => ({}),
  });
}

beforeEach(() => {
  document.body.replaceChildren();
  installCssEscape();
});

afterEach(() => {
  for (const manager of managers.splice(0)) manager.destroy();
  vi.restoreAllMocks();
});

describe('GptDiagnosticsBindingManager', () => {
  it('binds only the unique exact ID and never a prefix match', () => {
    const prefix = document.createElement('div');
    prefix.id = 'ad-slot-1-extra';
    const exact = document.createElement('div');
    exact.id = 'ad-slot-1';
    document.body.append(prefix, exact);
    setRectangle(exact, { top: 10, left: 10, width: 300, height: 250 });
    const originalClass = exact.className;
    const originalStyle = exact.getAttribute('style');
    const originalAttributes = exact.getAttributeNames();
    const store = createStore();
    store.recordSlotRequested(fakeSlot('ad-slot-1'));

    const manager = createManager(store);
    const view = manager.get(1);

    expect(view.binding).toEqual({ status: 'bound' });
    expect(view.element).toBe(exact);
    expect(view.visible).toBe(true);
    expect(exact.className).toBe(originalClass);
    expect(exact.getAttribute('style')).toBe(originalStyle);
    expect(exact.getAttributeNames()).toEqual(originalAttributes);
  });

  it('reports a missing exact element as unbound', () => {
    const prefix = document.createElement('div');
    prefix.id = 'missing-extra';
    document.body.append(prefix);
    const store = createStore();
    store.recordSlotRequested(fakeSlot('missing'));

    const manager = createManager(store);

    expect(manager.get(1)).toMatchObject({
      binding: { status: 'unbound', reason: 'missing_element' },
      visible: false,
    });
  });

  it('reports an empty GPT element ID as unbound without a synthetic DOM ID', () => {
    const store = createStore();
    store.recordSlotRequested(fakeSlot());

    const manager = createManager(store);

    expect(manager.get(1).binding).toEqual({
      status: 'unbound',
      reason: 'missing_slot_element_id',
    });
    expect(store.snapshot().slots[0]!.slotElementId).toBeUndefined();
  });

  it('treats duplicate DOM IDs as ambiguous', () => {
    const first = document.createElement('div');
    first.id = 'duplicate';
    const second = document.createElement('div');
    second.id = 'duplicate';
    document.body.append(first, second);
    const store = createStore();
    store.recordSlotRequested(fakeSlot('duplicate'));

    const manager = createManager(store);

    expect(manager.get(1)).toMatchObject({
      binding: { status: 'ambiguous', reason: 'duplicate_dom_id' },
      visible: false,
    });
  });

  it('treats duplicate retained GPT slot IDs as ambiguous', () => {
    const element = document.createElement('div');
    element.id = 'shared';
    document.body.append(element);
    const store = createStore();
    store.recordSlotRequested(fakeSlot('shared'));
    store.recordSlotRequested(fakeSlot('shared'));

    const manager = createManager(store);

    expect(manager.get(1).binding).toEqual({
      status: 'ambiguous',
      reason: 'duplicate_gpt_slot_id',
    });
    expect(manager.get(2).binding).toEqual({
      status: 'ambiguous',
      reason: 'duplicate_gpt_slot_id',
    });
  });

  it('rebinds after element replacement and becomes unbound after disconnection', () => {
    const first = document.createElement('div');
    first.id = 'replaceable';
    document.body.append(first);
    const store = createStore();
    store.recordSlotRequested(fakeSlot('replaceable'));
    const manager = createManager(store);
    expect(manager.get(1).element).toBe(first);

    const replacement = document.createElement('div');
    replacement.id = 'replaceable';
    first.replaceWith(replacement);
    manager.refresh();
    expect(manager.get(1).element).toBe(replacement);

    replacement.remove();
    manager.refresh();
    expect(manager.get(1).binding).toEqual({
      status: 'unbound',
      reason: 'missing_element',
    });
  });

  it('requires non-zero viewport intersection for DOM visibility', () => {
    const element = document.createElement('div');
    element.id = 'geometry';
    document.body.append(element);
    const store = createStore();
    store.recordSlotRequested(fakeSlot('geometry'));
    const manager = createManager(store);

    setRectangle(element, { top: 10, left: 10, width: 0, height: 250 });
    manager.refresh();
    expect(manager.get(1).visible).toBe(false);

    setRectangle(element, {
      top: window.innerHeight + 10,
      left: 10,
      width: 300,
      height: 250,
    });
    manager.refresh();
    expect(manager.get(1).visible).toBe(false);

    setRectangle(element, { top: 10, left: 10, width: 300, height: 250 });
    manager.refresh();
    expect(manager.get(1).visible).toBe(true);
  });

  it('does not guess when DOM uniqueness cannot be verified', () => {
    Object.defineProperty(window, 'CSS', { configurable: true, value: undefined });
    const element = document.createElement('div');
    element.id = 'unverifiable';
    document.body.append(element);
    const store = createStore();
    store.recordSlotRequested(fakeSlot('unverifiable'));

    const manager = createManager(store);

    expect(manager.get(1).binding).toEqual({
      status: 'ambiguous',
      reason: 'dom_uniqueness_unverifiable',
    });
  });

  it.each(['escape', 'selector'] as const)(
    'reports %s failures as unverifiable rather than an observed duplicate',
    (failure) => {
      const element = document.createElement('div');
      element.id = 'throws';
      document.body.append(element);
      if (failure === 'escape') {
        vi.spyOn(window.CSS, 'escape').mockImplementation(() => {
          throw new Error('escape failed');
        });
      } else {
        vi.spyOn(document, 'querySelectorAll').mockImplementation(() => {
          throw new Error('selector failed');
        });
      }
      const store = createStore();
      store.recordSlotRequested(fakeSlot('throws'));

      const manager = createManager(store);

      expect(manager.get(1)).toMatchObject({
        binding: { status: 'ambiguous', reason: 'dom_uniqueness_unverifiable' },
        visible: false,
      });
      expect(manager.get(1).element).toBeUndefined();
    }
  );

  it('ignores unrelated mutations and refreshes for known-ID replacements', async () => {
    const frames: Array<() => void> = [];
    const element = document.createElement('div');
    element.id = 'observed';
    document.body.append(element);
    const store = createStore();
    store.recordSlotRequested(fakeSlot('observed'));
    const manager = createManager(store, queueFrame(frames));

    const unrelated = document.createElement('div');
    unrelated.id = 'unrelated';
    document.body.append(unrelated);
    await Promise.resolve();
    expect(frames).toEqual([]);

    const replacement = document.createElement('div');
    replacement.id = 'observed';
    element.replaceWith(replacement);
    await Promise.resolve();
    expect(frames).toHaveLength(1);
    frames.shift()!();
    expect(manager.get(1).element).toBe(replacement);

    replacement.id = 'renamed';
    await Promise.resolve();
    expect(frames).toHaveLength(1);
    frames.shift()!();
    expect(manager.get(1).binding).toEqual({ status: 'unbound', reason: 'missing_element' });
  });

  it('coalesces store-driven refreshes to one animation frame', () => {
    const scheduled: Array<() => void> = [];
    const store = createStore();
    const manager = createManager(store, queueFrame(scheduled));
    const listener = vi.fn();
    manager.subscribe(listener);
    const slot = fakeSlot('scheduled');

    store.recordSlotRequested(slot);
    store.recordSlotVisibilityChanged(slot, 10);
    store.recordSlotVisibilityChanged(slot, 20);

    expect(scheduled).toHaveLength(1);
    scheduled.shift()!();
    expect(listener).toHaveBeenCalledTimes(1);
    expect(manager.get(1).binding).toEqual({
      status: 'unbound',
      reason: 'missing_element',
    });
  });

  it('cancels a pending refresh on destroy and suppresses a hostile late callback', () => {
    const frames: Array<() => void> = [];
    const cancel = vi.fn();
    const store = createStore();
    const manager = createManager(store, (callback) => {
      frames.push(callback);
      return cancel;
    });
    const listener = vi.fn();
    manager.subscribe(listener);
    store.recordSlotRequested(fakeSlot('pending-destroy'));

    manager.destroy();
    frames[0]?.();

    expect(cancel).toHaveBeenCalledOnce();
    expect(listener).not.toHaveBeenCalled();
  });

  it('runs one scheduled refresh callback at most once', () => {
    const frames: Array<() => void> = [];
    const store = createStore();
    const manager = createManager(store, (callback) => {
      frames.push(callback);
      return vi.fn();
    });
    const listener = vi.fn();
    manager.subscribe(listener);
    store.recordSlotRequested(fakeSlot('once'));

    frames[0]?.();
    frames[0]?.();

    expect(listener).toHaveBeenCalledOnce();
  });

  it('isolates a hostile frame cancellation during destroy', () => {
    const store = createStore();
    const cancel = vi.fn(() => {
      throw new Error('cancel failed');
    });
    const manager = createManager(store, () => cancel);
    store.recordSlotRequested(fakeSlot('hostile-cancel'));

    expect(() => manager.destroy()).not.toThrow();
    expect(cancel).toHaveBeenCalledOnce();
  });
});
