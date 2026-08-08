import type {
  IntegrationActivationContext,
  IntegrationPrepareContext,
  IntegrationRegistration,
} from '../../kernel/integration_registry';

import type { GptDiagnosticsRuntime } from './index';

export const GPT_DIAGNOSTICS_INTEGRATION_ID = 'gpt_diagnostics' as const;

function activeConfiguration(candidate: unknown): boolean {
  try {
    if (
      typeof candidate !== 'object' ||
      candidate === null ||
      Array.isArray(candidate) ||
      !Object.isFrozen(candidate) ||
      Object.getPrototypeOf(candidate) !== Object.prototype ||
      Reflect.ownKeys(candidate).length !== 1
    ) {
      return false;
    }
    const active = Object.getOwnPropertyDescriptor(candidate, 'active');
    return Boolean(active?.enumerable && 'value' in active && active.value === true);
  } catch {
    return false;
  }
}

function diagnosticsRuntime(
  interfaces: Readonly<Record<string, unknown>>
): GptDiagnosticsRuntime | undefined {
  try {
    const descriptor = Object.getOwnPropertyDescriptor(interfaces, GPT_DIAGNOSTICS_INTEGRATION_ID);
    if (!descriptor || !('value' in descriptor)) return undefined;
    const candidate = descriptor.value;
    if (
      typeof candidate !== 'object' ||
      candidate === null ||
      Array.isArray(candidate) ||
      !Object.isFrozen(candidate) ||
      Reflect.ownKeys(candidate).length !== 2
    ) {
      return undefined;
    }
    const activate = Object.getOwnPropertyDescriptor(candidate, 'activate');
    const currentApi = Object.getOwnPropertyDescriptor(candidate, 'currentApi');
    if (
      !activate?.enumerable ||
      !('value' in activate) ||
      typeof activate.value !== 'function' ||
      !currentApi?.enumerable ||
      !('value' in currentApi) ||
      typeof currentApi.value !== 'function'
    ) {
      return undefined;
    }
    return candidate as GptDiagnosticsRuntime;
  } catch {
    return undefined;
  }
}

/** Builds the release-bound GPT diagnostics module for the coordinated runtime. */
export function createGptDiagnosticsIntegrationRegistration(
  release: string
): IntegrationRegistration {
  return Object.freeze({
    id: GPT_DIAGNOSTICS_INTEGRATION_ID,
    release,
    prepare: ({ config, interfaces }: IntegrationPrepareContext) => {
      if (!activeConfiguration(config)) {
        throw new TypeError('GPT diagnostics integration config is invalid');
      }
      const runtime = diagnosticsRuntime(interfaces);
      if (!runtime) throw new TypeError('GPT diagnostics integration runtime is unavailable');

      return Object.freeze({
        activate: ({ onDispose }: IntegrationActivationContext) => {
          const ownership: { release?: () => void } = {};
          onDispose(() => ownership.release?.());
          const releaseRuntime = runtime.activate();
          if (typeof releaseRuntime !== 'function') {
            throw new TypeError('GPT diagnostics integration disposer is unavailable');
          }
          ownership.release = releaseRuntime;
        },
      });
    },
  });
}
