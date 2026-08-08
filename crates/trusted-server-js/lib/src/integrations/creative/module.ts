import type { CreativeBootV1 } from '../../core/types';
import type {
  IntegrationActivationContext,
  IntegrationPrepareContext,
  IntegrationRegistration,
} from '../../kernel/integration_registry';

export const CREATIVE_INTEGRATION_ID = 'creative' as const;

interface CreativeIntegrationRuntime {
  readonly activate: (config: Readonly<CreativeBootV1>) => () => void;
  readonly start: (config: Readonly<CreativeBootV1>) => void;
}

function readCreativeBoot(candidate: unknown): Readonly<CreativeBootV1> | undefined {
  try {
    if (
      typeof candidate !== 'object' ||
      candidate === null ||
      Array.isArray(candidate) ||
      !Object.isFrozen(candidate) ||
      Object.getPrototypeOf(candidate) !== Object.prototype ||
      Object.getOwnPropertySymbols(candidate).length !== 0
    ) {
      return undefined;
    }
    const keys = Object.getOwnPropertyNames(candidate).sort();
    const expected = ['clickGuard', 'enabled', 'renderGuard', 'version'];
    if (keys.length !== expected.length || keys.some((key, index) => key !== expected[index])) {
      return undefined;
    }
    const values: Record<string, unknown> = {};
    for (let index = 0; index < expected.length; index += 1) {
      const key = expected[index];
      if (!key) return undefined;
      const descriptor = Object.getOwnPropertyDescriptor(candidate, key);
      if (!descriptor || !descriptor.enumerable || !('value' in descriptor)) return undefined;
      values[key] = descriptor.value;
    }
    return values['version'] === 1 &&
      typeof values['enabled'] === 'boolean' &&
      typeof values['clickGuard'] === 'boolean' &&
      typeof values['renderGuard'] === 'boolean'
      ? (candidate as Readonly<CreativeBootV1>)
      : undefined;
  } catch {
    return undefined;
  }
}

function readCreativeRuntime(
  interfaces: Readonly<Record<string, unknown>>
): CreativeIntegrationRuntime | undefined {
  try {
    const descriptor = Object.getOwnPropertyDescriptor(interfaces, CREATIVE_INTEGRATION_ID);
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
    const start = Object.getOwnPropertyDescriptor(candidate, 'start');
    if (
      !activate ||
      !('value' in activate) ||
      typeof activate.value !== 'function' ||
      !start ||
      !('value' in start) ||
      typeof start.value !== 'function'
    ) {
      return undefined;
    }
    return candidate as CreativeIntegrationRuntime;
  } catch {
    return undefined;
  }
}

/** Build the inert, release-bound creative module for the coordinated runtime. */
export function createCreativeIntegrationRegistration(release: string): IntegrationRegistration {
  return Object.freeze({
    id: CREATIVE_INTEGRATION_ID,
    release,
    prepare: async ({ config, interfaces }: IntegrationPrepareContext) => {
      const creative = readCreativeBoot(config);
      if (!creative) throw new TypeError('Creative boot configuration is invalid');
      const runtime = readCreativeRuntime(interfaces);
      if (!runtime) throw new TypeError('Creative integration runtime is unavailable');
      if (!creative.enabled || (!creative.clickGuard && !creative.renderGuard)) {
        return Object.freeze({ activate: () => undefined });
      }

      return Object.freeze({
        activate: ({ afterCommit, onDispose }: IntegrationActivationContext) => {
          const runtimeRelease: { value?: () => void } = {};
          onDispose(() => runtimeRelease.value?.());
          const releaseRuntime = runtime.activate(creative);
          if (typeof releaseRuntime !== 'function') {
            throw new TypeError('Creative integration activation disposer is unavailable');
          }
          runtimeRelease.value = releaseRuntime;
          afterCommit(() => runtime.start(creative));
        },
      });
    },
  });
}
