import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import {
  claimFirstImpressionForTrustedServer,
  consumePublisherFirstImpressionDelivery,
  observeFirstImpressionGptLifecycle,
  registerPublisherFirstImpressionAuctions,
  releaseTrustedServerFirstImpressionClaim,
} from '../../src/core/first_impression';
import { log } from '../../src/core/log';
import type { TsjsApi } from '../../src/core/types';

describe('initial render capacity and diagnostics', () => {
  beforeEach(() => {
    vi.useFakeTimers();
    document.body.innerHTML = '<div id="example-slot"></div>';
  });
  afterEach(() => {
    vi.clearAllTimers();
    vi.useRealTimers();
    document.body.replaceChildren();
  });

  it('suppresses overflow through settlement without growing the token map', () => {
    const ts: TsjsApi = {};
    const element = document.getElementById('example-slot')!;
    const claim = claimFirstImpressionForTrustedServer(ts, element)!;
    observeFirstImpressionGptLifecycle(ts, element, 'requested');
    const tokens: string[] = [];
    for (let index = 0; index < 40; index++) {
      const token = registerPublisherFirstImpressionAuctions(ts, [element.id]).get(element.id);
      expect(token).toBeDefined();
      tokens.push(token!);
      expect(consumePublisherFirstImpressionDelivery(ts, token)).toBe(true);
    }
    expect(Object.keys(claim.publisherAuctions)).toHaveLength(16);
    observeFirstImpressionGptLifecycle(ts, element, 'rendered');
    expect(registerPublisherFirstImpressionAuctions(ts, [element.id]).size).toBe(0);
    expect(consumePublisherFirstImpressionDelivery(ts, tokens.at(-1))).toBe(true);

    element.remove();
    document.body.innerHTML = '<div id="example-slot"></div>';
    const replacement = document.getElementById('example-slot')!;
    const replacementClaim = claimFirstImpressionForTrustedServer(ts, replacement)!;
    const newToken = registerPublisherFirstImpressionAuctions(ts, [replacement.id]).get(
      replacement.id
    );
    expect(newToken).not.toBe(tokens.at(-1));
    expect(consumePublisherFirstImpressionDelivery(ts, tokens.at(-1))).toBe(false);
    expect(replacementClaim.publisherAuctions[newToken!]).toBeDefined();
    ts.navGeneration = 1;
    expect(consumePublisherFirstImpressionDelivery(ts, newToken)).toBe(false);
  });

  it.each(['delivery_pending', 'requested'] as const)(
    'records one pending snapshot for %s without unlocking',
    (phase) => {
      const debug = vi.fn();
      const ts: TsjsApi = { log: { ...log, debug } };
      const element = document.getElementById('example-slot')!;
      const claim = claimFirstImpressionForTrustedServer(ts, element)!;
      if (phase === 'requested') observeFirstImpressionGptLifecycle(ts, element, phase);
      vi.advanceTimersByTime(4999);
      expect(debug).not.toHaveBeenCalled();
      vi.advanceTimersByTime(1);
      expect(debug).toHaveBeenCalledOnce();
      expect(debug).toHaveBeenCalledWith('[tsjs-gpt] initial render remains pending', {
        slot: element.id,
        generation: 0,
        phase,
        ageMs: 5000,
      });
      expect(claim).toHaveProperty('pendingRenderDiagnostic', { phase, ageMs: 5000 });
      vi.advanceTimersByTime(30000);
      expect(debug).toHaveBeenCalledOnce();
      const token = registerPublisherFirstImpressionAuctions(ts, [element.id]).get(element.id);
      expect(consumePublisherFirstImpressionDelivery(ts, token)).toBe(true);
    }
  );

  it.each(['render', 'release', 'replace', 'navigate'])(
    'ignores a diagnostic timer after %s',
    (action) => {
      const debug = vi.fn();
      const ts: TsjsApi = { log: { ...log, debug } };
      const element = document.getElementById('example-slot')!;
      const claim = claimFirstImpressionForTrustedServer(ts, element)!;
      if (action === 'render') observeFirstImpressionGptLifecycle(ts, element, 'rendered');
      if (action === 'release') releaseTrustedServerFirstImpressionClaim(ts, element, claim);
      if (action === 'replace') document.body.innerHTML = '<div id="example-slot"></div>';
      if (action === 'navigate') ts.navGeneration = 1;
      vi.advanceTimersByTime(5000);
      expect(debug).not.toHaveBeenCalled();
      expect(claim).not.toHaveProperty('pendingRenderDiagnostic');
    }
  );

  it.each([false, true])(
    'preserves admission with a missing or throwing logger (throws=%s)',
    (throws) => {
      const ts: TsjsApi = throws
        ? {
            log: {
              ...log,
              debug: () => {
                throw new Error('example logger failure');
              },
            },
          }
        : {};
      const element = document.getElementById('example-slot')!;
      const claim = claimFirstImpressionForTrustedServer(ts, element)!;
      expect(() => vi.advanceTimersByTime(5000)).not.toThrow();
      expect(claim).toHaveProperty('pendingRenderDiagnostic');
      const token = registerPublisherFirstImpressionAuctions(ts, [element.id]).get(element.id);
      expect(consumePublisherFirstImpressionDelivery(ts, token)).toBe(true);
    }
  );
});
