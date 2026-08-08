export async function waitForExpect(assertion: () => void, timeout = 200): Promise<void> {
  const start = Date.now();
  for (;;) {
    try {
      assertion();
      return;
    } catch (err) {
      if (Date.now() - start >= timeout) throw err;
    }
    await new Promise((resolve) => setTimeout(resolve, 5));
  }
}

export const FIRST_PARTY_CLICK =
  '/first-party/click?tsurl=https%3A%2F%2Fexample.com%2Flanding&foo=1&tstoken=token123';
export const MUTATED_CLICK = 'https://example.com/landing?bar=2';
export const PROXY_RESPONSE =
  '/first-party/click?tsurl=https%3A%2F%2Fexample.com%2Flanding&bar=2&tstoken=newtoken';

import type { TsCreativeConfig } from '../../../src/shared/globals';

let disposeLastImportedCreative: (() => void) | undefined;

export function disposeImportedCreativeModule(): void {
  const dispose = disposeLastImportedCreative;
  disposeLastImportedCreative = undefined;
  dispose?.();
}

export async function importCreativeModule(config?: TsCreativeConfig): Promise<void> {
  disposeImportedCreativeModule();
  const globalRef = globalThis as {
    __ts_creative_installed?: boolean;
    tsCreativeConfig?: TsCreativeConfig;
  };
  delete globalRef.__ts_creative_installed;
  if (config) {
    globalRef.tsCreativeConfig = config;
  }
  const creative = await import('../../../src/integrations/creative/index');
  disposeLastImportedCreative = creative.disposeGuards;
  if (config) {
    delete globalRef.tsCreativeConfig;
  }
}
