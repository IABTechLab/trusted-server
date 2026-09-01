import { describe, expect, it, vi } from 'vitest';

import { createOsanoRuntime } from '../../../src/integrations/osano/module';

describe('transactional Osano integration module', () => {
  it('keeps activation reversible and starts the consent mirror once after commit', () => {
    const initialize = vi.fn();
    const reset = vi.fn();
    const runtime = createOsanoRuntime({ initialize, reset });

    const release = runtime.activate(undefined);

    expect(initialize).not.toHaveBeenCalled();
    runtime.start(undefined);
    runtime.start(undefined);
    expect(initialize).toHaveBeenCalledOnce();
    release();
    release();
    expect(reset).toHaveBeenCalledOnce();
  });

  it('resets partial consent ownership when startup throws', () => {
    const reset = vi.fn();
    const runtime = createOsanoRuntime({
      initialize: () => {
        throw new Error('listener failed');
      },
      reset,
    });
    const release = runtime.activate(undefined);

    expect(() => runtime.start(undefined)).toThrow('listener failed');
    release();

    expect(reset).toHaveBeenCalledOnce();
  });
});
