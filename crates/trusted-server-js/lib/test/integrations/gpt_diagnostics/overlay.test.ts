import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { log } from '../../../src/core/log';
import type { GptDiagnosticsBinding } from '../../../src/core/types';
import type { GptDiagnosticsBindingView } from '../../../src/integrations/gpt_diagnostics/binding';
import {
  GPT_DIAGNOSTICS_HOST_ID,
  GptDiagnosticsOverlay,
} from '../../../src/integrations/gpt_diagnostics/overlay';
import {
  GptDiagnosticsStore,
  TRUSTED_SERVER_ATTRIBUTION_WINDOW_MS,
} from '../../../src/integrations/gpt_diagnostics/store';

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

function requireAttempt(attemptId: number | undefined): number {
  if (attemptId === undefined) throw new Error('Missing Trusted Server creative attempt');
  return attemptId;
}

function slotArticle(root: ShadowRoot, slotElementId: string): HTMLElement {
  const article = Array.from(root.querySelectorAll<HTMLElement>('.tsgd-slot')).find((candidate) =>
    candidate.textContent?.includes(slotElementId)
  );
  if (!article) throw new Error(`Missing ${slotElementId} diagnostics article`);
  return article;
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

  it('presents the request path and Trusted Server delivery evidence without winner claims', () => {
    const frames: Array<() => void> = [];
    let now = 10;
    const store = new GptDiagnosticsStore({
      now: () => now,
      schedule: (callback) => callback(),
      defer: () => undefined,
    });
    const bindings = new FakeBindings();

    const responseSentSlot = slot('response-sent-slot');
    store.recordTrustedServerOpportunity(
      responseSentSlot,
      'auction-response-sent',
      'renderable_candidate',
      'auction-123'
    );
    store.recordSlotRequested(responseSentSlot);
    now = 11;
    store.recordSlotResponseReceived(responseSentSlot);
    now = 12;
    store.recordSlotRenderEnded(responseSentSlot, {
      isEmpty: false,
      adManager: {
        lineItemId: 6543210987,
        campaignId: 2345678901,
        creativeId: 3456789012,
      },
    });
    now = 13;
    const responseSentAttempt = requireAttempt(
      store.recordTrustedServerCreativeRequest('auction-response-sent')
    );
    store.recordTrustedServerCreativeFailure(responseSentAttempt, 'missing_render_source');
    store.recordTrustedServerCreativeFailure(responseSentAttempt, 'missing_render_source');
    store.recordTrustedServerCreativeFailure(responseSentAttempt, 'cache_fetch_failed');
    store.recordTrustedServerCreativeFailure(responseSentAttempt, 'invalid_cache_payload');
    store.recordTrustedServerCreativeFailure(responseSentAttempt, 'response_post_failed');
    now = 14;
    store.recordTrustedServerCreativeResponse(responseSentAttempt);
    now = 15;
    store.recordSlotOnload(responseSentSlot);
    now = 16;
    store.recordImpressionViewable(responseSentSlot);

    const selectedSlot = slot('selected-slot');
    now = 20;
    store.recordTrustedServerOpportunity(
      selectedSlot,
      'auction-selected',
      'unrenderable_candidate'
    );
    store.recordPrebidRefresh([selectedSlot]);
    store.recordSlotRequested(selectedSlot);
    now = 21;
    store.recordSlotResponseReceived(selectedSlot);
    now = 22;
    store.recordSlotRenderEnded(selectedSlot, { isEmpty: false });
    now = 23;
    requireAttempt(store.recordTrustedServerCreativeRequest('auction-selected'));

    const noCandidateSlot = slot('no-candidate-slot');
    now = 30;
    store.recordTrustedServerOpportunity(noCandidateSlot, 'auction-no-candidate', 'no_candidate');
    store.recordSlotRequested(noCandidateSlot);
    now = 31;
    store.recordSlotResponseReceived(noCandidateSlot);
    now = 32;
    store.recordSlotRenderEnded(noCandidateSlot, { isEmpty: false });

    const unattributedSlot = slot('unattributed-slot');
    now = 40;
    store.recordSlotRequested(unattributedSlot);
    now = 41;
    store.recordSlotResponseReceived(unattributedSlot);
    now = 42;
    store.recordSlotRenderEnded(unattributedSlot, {});

    const prebidSlot = slot('prebid-slot');
    now = 50;
    store.recordPrebidRefresh([prebidSlot]);
    store.recordSlotRequested(prebidSlot);
    now = 51;
    store.recordSlotResponseReceived(prebidSlot);
    now = 52;
    store.recordSlotRenderEnded(prebidSlot, { isEmpty: false });

    const unconfirmedSlot = slot('unconfirmed-slot');
    now = 100;
    store.recordTrustedServerOpportunity(
      unconfirmedSlot,
      'auction-unconfirmed',
      'renderable_candidate'
    );
    store.recordSlotRequested(unconfirmedSlot);
    now = 101;
    store.recordSlotResponseReceived(unconfirmedSlot);
    now = 102;
    store.recordSlotRenderEnded(unconfirmedSlot, { isEmpty: false });

    const pendingSlot = slot('candidate-pending-slot');
    now = 102 + TRUSTED_SERVER_ATTRIBUTION_WINDOW_MS;
    store.recordTrustedServerOpportunity(pendingSlot, 'auction-pending', 'renderable_candidate');
    store.recordSlotRequested(pendingSlot);
    now += 1;
    store.recordSlotResponseReceived(pendingSlot);
    now += 1;
    store.recordSlotRenderEnded(pendingSlot, { isEmpty: false });

    const notApplicableSlot = slot('not-applicable-slot');
    now += 1;
    store.recordTrustedServerOpportunity(
      notApplicableSlot,
      'auction-not-applicable',
      'renderable_candidate'
    );
    store.recordSlotRequested(notApplicableSlot);

    store.recordTrustedServerCreativeRequest('missing-auction-slot');
    store.recordSlotRenderEnded(slot('callback-issue-slot'), { isEmpty: false });

    let root: ShadowRoot | undefined;
    const overlay = new GptDiagnosticsOverlay(store, bindings, {
      scheduleFrame: (callback) => frames.push(callback),
      onShadowRoot: (createdRoot) => {
        root = createdRoot;
      },
    });
    runNextFrame(frames);
    runNextFrame(frames);

    const text = root!.textContent ?? '';
    const responseSentArticle = slotArticle(root!, 'response-sent-slot').textContent ?? '';
    expect(responseSentArticle).toContain('Request path: Trusted Server direct');
    expect(responseSentArticle).toContain('Request intent: 1');
    expect(responseSentArticle).toContain('Trusted Server auction: auction-123');
    expect(responseSentArticle).toContain('Opportunity → request 0 ms');
    expect(responseSentArticle).toContain('Direct opportunity: Renderable candidate');
    expect(responseSentArticle).toContain('Trusted Server creative request observed at 13 ms');
    expect(responseSentArticle).toContain('Trusted Server markup response sent at 14 ms');
    expect(
      responseSentArticle.match(/Creative bridge failure: missing render source/g)
    ).toHaveLength(1);
    expect(responseSentArticle).toContain('Creative bridge failure: cache fetch failed');
    expect(responseSentArticle).toContain('Creative bridge failure: invalid cache payload');
    expect(responseSentArticle).toContain('Creative bridge failure: response post failed');
    expect(responseSentArticle).toContain('Trusted Server selected; markup response sent to PUC');
    expect(responseSentArticle).toContain(
      'Ad Manager reported line item 6543210987 · order 2345678901'
    );
    expect(responseSentArticle).toContain('Ad Manager reservation line item');
    expect(responseSentArticle).toContain('GPT slot onload observed');
    expect(responseSentArticle).toContain('GPT impressionViewable observed');
    expect(responseSentArticle).not.toMatch(/creative rendered|ad visible|pixels confirmed/i);

    const selectedArticle = slotArticle(root!, 'selected-slot').textContent ?? '';
    expect(selectedArticle).toContain('Request path: Competing paths');
    expect(selectedArticle).toContain('Direct opportunity: Unrenderable candidate');
    expect(selectedArticle).toContain('Trusted Server creative request observed at 23 ms');
    expect(selectedArticle).not.toContain('Trusted Server markup response sent');
    expect(selectedArticle).toContain('Trusted Server selected; no markup response confirmed');

    const noCandidateArticle = slotArticle(root!, 'no-candidate-slot').textContent ?? '';
    expect(noCandidateArticle).toContain('Request path: Trusted Server direct');
    expect(noCandidateArticle).toContain('Direct opportunity: No candidate');
    expect(noCandidateArticle).toContain(
      'adInit observed no direct Trusted Server candidate for this request'
    );

    const unattributedArticle = slotArticle(root!, 'unattributed-slot').textContent ?? '';
    expect(unattributedArticle).toContain('Request path: Unattributed');
    expect(unattributedArticle).toContain('Direct opportunity: Unknown (not observed)');
    expect(unattributedArticle).toContain(
      'Delivery status unknown — required GPT or direct-candidate evidence was not observed'
    );

    const prebidArticle = slotArticle(root!, 'prebid-slot').textContent ?? '';
    expect(prebidArticle).toContain('Request path: Prebid refresh');
    expect(prebidArticle).toContain('Direct opportunity: Unknown (not observed)');
    expect(prebidArticle).toContain(
      'Delivery status unknown — required GPT or direct-candidate evidence was not observed'
    );

    const unconfirmedArticle = slotArticle(root!, 'unconfirmed-slot').textContent ?? '';
    expect(unconfirmedArticle).toContain('Request path: Trusted Server direct');
    expect(unconfirmedArticle).toContain('Direct opportunity: Renderable candidate');
    expect(unconfirmedArticle).toContain(
      'Trusted Server candidate unconfirmed — another GAM result or a creative/bridge failure is possible'
    );

    const pendingArticle = slotArticle(root!, 'candidate-pending-slot').textContent ?? '';
    expect(pendingArticle).toContain('Request path: Trusted Server direct');
    expect(pendingArticle).toContain('Direct opportunity: Renderable candidate');
    expect(pendingArticle).toContain('Waiting for Trusted Server creative evidence');

    const notApplicableArticle = slotArticle(root!, 'not-applicable-slot').textContent ?? '';
    expect(notApplicableArticle).toContain('Request path: Trusted Server direct');
    expect(notApplicableArticle).toContain('Direct opportunity: Renderable candidate');
    expect(notApplicableArticle).not.toMatch(
      /Trusted Server selected|candidate unconfirmed|no direct Trusted Server candidate|Delivery status unknown|Waiting for Trusted Server creative evidence/
    );

    expect(text).toContain(
      `${store.snapshot().slots.length} slots · 1 callback issues · 1 attribution issues`
    );
    expect(text).not.toMatch(
      /creative rendered|other demand won|no Trusted Server creative ran|ad visible|pixels confirmed/i
    );
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
    expect(root!.textContent).toContain('attribution issues');
    expect(root!.textContent).toContain('filled-slot');
    expect(root!.textContent).toContain('/example/site/filled-slot');
    expect(root!.textContent).toContain('Empty');
    expect(root!.textContent).toContain('Previous requests (1)');
    expect(root!.textContent).toContain('Rendered size 300×250');
    expect(root!.textContent).toContain('Backfill yes');
    expect(root!.textContent).toContain('GPT slot onload observed');
    expect(root!.textContent).toContain('GPT impressionViewable observed');
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
