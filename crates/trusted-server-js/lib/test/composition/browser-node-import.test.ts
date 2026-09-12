// @vitest-environment node

import { describe, expect, it } from 'vitest';

describe('browser composition in a non-DOM runtime', () => {
  it('imports without claiming browser globals and constructs the no-op composition', async () => {
    expect(globalThis.document).toBeUndefined();

    const { createNoopBrowserComposition } = await import('../../src/composition/browser_test');
    const composition = createNoopBrowserComposition();

    expect(Object.isFrozen(composition)).toBe(true);
    expect(Object.isFrozen(composition.adapters)).toBe(true);
    expect(composition.adapters.messaging.createChannel()).toBeUndefined();
  });
});
