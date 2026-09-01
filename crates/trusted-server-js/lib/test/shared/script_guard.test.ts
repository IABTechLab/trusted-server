import { afterEach, describe, expect, it, vi } from 'vitest';

import { createScriptGuard } from '../../src/shared/script_guard';

describe('shared layered script guard', () => {
  const guards: Array<{ reset(): void }> = [];

  afterEach(() => {
    for (let index = guards.length - 1; index >= 0; index -= 1) guards[index]?.reset();
    guards.length = 0;
  });

  it('owns document-write rewriting and restores the exact native method', () => {
    const nativeWrite = vi.fn<(...args: string[]) => void>();
    document.write = nativeWrite as unknown as typeof document.write;
    const guard = createScriptGuard({
      deepInterception: { documentWriteUrlHint: 'sdk.example' },
      id: 'shared-layered-test',
      isTargetUrl: (url) => new URL(url, window.location.href).hostname === 'sdk.example',
      rewriteUrl: (url) => {
        const parsed = new URL(url, window.location.href);
        return `${window.location.origin}/proxy${parsed.pathname}`;
      },
    });
    guards.push(guard);

    guard.install();
    const installedWrite = document.write;
    document.write('<script src="https://sdk.example/runtime.js"></script>');

    expect(nativeWrite).toHaveBeenCalledTimes(1);
    expect(nativeWrite.mock.calls[0]?.[0]).toContain('/proxy/runtime.js');

    guard.reset();
    expect(document.write).toBe(nativeWrite);
    expect(installedWrite).not.toBe(nativeWrite);
  });

  it('removes fallback instance src descriptors during reset', () => {
    const nativeGetOwnPropertyDescriptor = Object.getOwnPropertyDescriptor;
    const descriptorSpy = vi
      .spyOn(Object, 'getOwnPropertyDescriptor')
      .mockImplementation(
        (target: object, property: PropertyKey): PropertyDescriptor | undefined => {
          if (target === HTMLScriptElement.prototype && property === 'src') return undefined;
          return nativeGetOwnPropertyDescriptor(target, property);
        }
      );
    const guard = createScriptGuard({
      deepInterception: { documentWriteUrlHint: 'sdk.example' },
      id: 'shared-layered-instance-test',
      isTargetUrl: (url) => new URL(url, window.location.href).hostname === 'sdk.example',
      rewriteUrl: (url) => {
        const parsed = new URL(url, window.location.href);
        return `${window.location.origin}/proxy${parsed.pathname}`;
      },
    });
    guards.push(guard);

    try {
      guard.install();
      const script = document.createElement('script');
      script.src = 'https://sdk.example/first.js';
      expect(script.src).toContain('/proxy/first.js');

      guard.reset();
      script.src = 'https://sdk.example/after-reset.js';
      expect(script.src).toBe('https://sdk.example/after-reset.js');
    } finally {
      descriptorSpy.mockRestore();
    }
  });
});
