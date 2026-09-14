import { startProductionRuntime } from '../core/index';
import { EMBEDDED_RELEASE_ID, EMBEDDED_RUNTIME_CATALOG } from '../core/release';
import { createRenderRuntimeIntegrationRegistration } from '../integrations/render_runtime/module';
import { createRuntime, type RuntimeOptions } from '../kernel/runtime';

const createRuntimeComposition = (runtimeOptions: RuntimeOptions) =>
  Object.freeze({
    runtime: createRuntime({
      ...runtimeOptions,
      catalog: EMBEDDED_RUNTIME_CATALOG,
    }),
  });

if (typeof window !== 'undefined' && typeof document !== 'undefined') {
  startProductionRuntime(createRuntimeComposition);
  const register = (window.tsjs as unknown as { _registerIntegration?: unknown } | undefined)
    ?._registerIntegration;
  if (typeof register === 'function') {
    Reflect.apply(register, window.tsjs, [
      createRenderRuntimeIntegrationRegistration(EMBEDDED_RELEASE_ID),
    ]);
  }
}
