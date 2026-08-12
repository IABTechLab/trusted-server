import { EMBEDDED_RUNTIME_CATALOG } from '../core/release';
import { createRuntime, type Runtime, type RuntimeOptions } from '../kernel/runtime';

export interface BrowserRuntimeComposition {
  readonly runtime: Runtime;
}

/** Minimal production core: concrete product behavior belongs to provider IIFEs. */
export function createBrowserRuntimeComposition(
  runtimeOptions: RuntimeOptions,
  _compositionOptions: Readonly<Record<string, never>> = Object.freeze({})
): BrowserRuntimeComposition {
  return Object.freeze({
    runtime: createRuntime({
      ...runtimeOptions,
      catalog: EMBEDDED_RUNTIME_CATALOG,
    }),
  });
}
