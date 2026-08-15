import type {
  IntegrationActivationContext,
  IntegrationPrepareContext,
  IntegrationRegistration,
} from '../../src/kernel/integration_registry';
import { installGptGuard, resetGuardState } from '../../src/integrations/gpt/script_guard';

interface TestGptRuntime {
  readonly activate: () => () => void;
  readonly start: (config: unknown) => void;
}

function recursivelyFrozen(candidate: unknown, seen = new Set<object>()): boolean {
  if (candidate === null || (typeof candidate !== 'object' && typeof candidate !== 'function')) {
    return typeof candidate !== 'number' || Number.isFinite(candidate);
  }
  if (seen.has(candidate) || !Object.isFrozen(candidate)) return false;
  const prototype = Object.getPrototypeOf(candidate);
  if (
    prototype !== Object.prototype &&
    prototype !== null &&
    !(Array.isArray(candidate) && prototype === Array.prototype)
  ) {
    return false;
  }
  seen.add(candidate);
  try {
    return Reflect.ownKeys(candidate).every((key) => {
      const descriptor = Object.getOwnPropertyDescriptor(candidate, key);
      return (
        descriptor !== undefined &&
        'value' in descriptor &&
        recursivelyFrozen(descriptor.value, seen)
      );
    });
  } catch {
    return false;
  }
}

function runtime(interfaces: Readonly<Record<string, unknown>>): TestGptRuntime | undefined {
  const candidate = interfaces['gpt'];
  if (
    typeof candidate !== 'object' ||
    candidate === null ||
    !Object.isFrozen(candidate) ||
    typeof (candidate as TestGptRuntime).activate !== 'function' ||
    typeof (candidate as TestGptRuntime).start !== 'function'
  ) {
    return undefined;
  }
  return candidate as TestGptRuntime;
}

/** Legacy composition seam retained only in tests; never reachable from a shipped entry point. */
export function createLegacyGptRegistrationForTest(releaseId: string): IntegrationRegistration {
  return Object.freeze({
    abi: 1,
    id: 'gpt',
    phase: 'takeover',
    releaseId,
    prepare: ({ config, interfaces }: IntegrationPrepareContext) => {
      if (!recursivelyFrozen(config)) throw new TypeError('GPT integration config is invalid');
      const preparedRuntime = runtime(interfaces);
      if (!preparedRuntime) throw new TypeError('GPT integration runtime is unavailable');
      return Object.freeze({
        activate: ({ afterCommit, onDispose }: IntegrationActivationContext) => {
          onDispose(resetGuardState);
          const releaseHolder: { current?: () => void } = {};
          onDispose(() => releaseHolder.current?.());
          const release = preparedRuntime.activate();
          if (typeof release !== 'function') {
            throw new TypeError('GPT integration activation disposer is unavailable');
          }
          releaseHolder.current = release;
          installGptGuard();
          afterCommit(() => preparedRuntime.start(config));
        },
      });
    },
  });
}
