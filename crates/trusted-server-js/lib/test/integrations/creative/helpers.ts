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

import type { CreativeBootV1 } from '../../../src/core/types';

let disposeLastImportedCreative: (() => void) | undefined;

export function disposeImportedCreativeModule(): void {
  const dispose = disposeLastImportedCreative;
  disposeLastImportedCreative = undefined;
  dispose?.();
}

export async function activateCreativeRuntime(
  config: Partial<Pick<CreativeBootV1, 'clickGuard' | 'renderGuard'>> = {}
): Promise<void> {
  disposeImportedCreativeModule();
  const [
    { installClickGuard },
    { installDynamicIframeProxy },
    { installDynamicImageProxy },
    startup,
  ] = await Promise.all([
    import('../../../src/integrations/creative/click'),
    import('../../../src/integrations/creative/iframe'),
    import('../../../src/integrations/creative/image'),
    import('../../../src/integrations/creative/startup'),
  ]);
  const boot = Object.freeze({
    version: 1 as const,
    enabled: true,
    clickGuard: config.clickGuard ?? true,
    renderGuard: config.renderGuard ?? false,
  });
  const runtime = startup.createCreativeStartup({
    document,
    installClickGuard: () => installClickGuard(false),
    installDynamicIframeProxy: () => installDynamicIframeProxy(false),
    installDynamicImageProxy: () => installDynamicImageProxy(false),
  });
  disposeLastImportedCreative = runtime.activate(boot);
  runtime.start(boot);
}
