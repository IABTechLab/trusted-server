import { startProductionRuntime } from '../core/index';
import { EMBEDDED_RELEASE_ID } from '../core/release';
import { createRenderRuntimeIntegrationRegistration } from '../integrations/render_runtime/module';

import { createBrowserRuntimeComposition } from './browser';

if (typeof window !== 'undefined' && typeof document !== 'undefined') {
  startProductionRuntime(createBrowserRuntimeComposition);
  const register = (window.tsjs as unknown as { _registerIntegration?: unknown } | undefined)
    ?._registerIntegration;
  if (typeof register === 'function') {
    Reflect.apply(register, window.tsjs, [
      createRenderRuntimeIntegrationRegistration(EMBEDDED_RELEASE_ID),
    ]);
  }
}
