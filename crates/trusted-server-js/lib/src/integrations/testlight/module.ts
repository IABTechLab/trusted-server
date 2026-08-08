import type { IntegrationRegistration } from '../../kernel/integration_registry';
import {
  createLifecycleIntegrationRegistration,
  type IntegrationLifecycleRuntime,
} from '../../kernel/lifecycle_module';
import { log } from '../../core/log';

export const TESTLIGHT_INTEGRATION_ID = 'testlight' as const;

interface TestlightGlobal {
  que?: unknown[] | undefined;
}

interface TestlightTarget {
  testlight?: TestlightGlobal | undefined;
}

export interface TestlightRuntimeDependencies {
  readonly enqueue: (callback: () => void) => void;
  readonly started: () => void;
  readonly target: TestlightTarget;
}

function callableQueue(candidate: unknown): candidate is { push: (entry: unknown) => number } {
  return (
    (typeof candidate === 'object' || typeof candidate === 'function') &&
    candidate !== null &&
    typeof (candidate as { push?: unknown }).push === 'function'
  );
}

function ownQueueValues(candidate: unknown): unknown[] {
  if (!Array.isArray(candidate)) return [];
  const entries: Array<readonly [number, unknown]> = [];
  try {
    for (const key of Reflect.ownKeys(candidate)) {
      if (typeof key !== 'string' || !/^(0|[1-9][0-9]*)$/.test(key)) continue;
      const index = Number(key);
      if (!Number.isSafeInteger(index) || index < 0 || index >= 4_294_967_295) continue;
      const descriptor = Object.getOwnPropertyDescriptor(candidate, key);
      if (descriptor && descriptor.enumerable && 'value' in descriptor) {
        entries.push([index, descriptor.value]);
      }
    }
  } catch {
    return [];
  }
  entries.sort(([left], [right]) => left - right);
  return entries.map(([, value]) => value);
}

/** Own Testlight's callback bridge without retaining callbacks after TSJS commit. */
export function createTestlightRuntime(
  dependencies: TestlightRuntimeDependencies = {
    enqueue: (callback) => {
      const queue = (window as typeof window & { tsjs?: { que?: unknown } }).tsjs?.que;
      if (!callableQueue(queue)) throw new Error('Testlight TSJS queue is unavailable');
      queue.push(callback);
    },
    started: () => log.info('Testlight integration initialized'),
    target: window as typeof window & TestlightTarget,
  }
): IntegrationLifecycleRuntime {
  let active = false;
  let started = false;
  let ownedGlobal: TestlightGlobal | undefined;
  let installedQueue: unknown[] | undefined;
  let previousQueueDescriptor: PropertyDescriptor | undefined;
  let previousTargetDescriptor: PropertyDescriptor | undefined;
  let createdGlobal = false;
  let forwarding = false;
  let originalQueue: unknown[] | undefined;
  let originalQueueLength = 0;

  const releaseOwnership = (): void => {
    const global = ownedGlobal;
    const queue = installedQueue;
    const queueDescriptor = previousQueueDescriptor;
    const targetDescriptor = previousTargetDescriptor;
    const removeGlobal = createdGlobal;
    const restoreQueue = originalQueue;
    const restoreQueueLength = originalQueueLength;
    const shouldReturnPending = !forwarding;
    ownedGlobal = undefined;
    installedQueue = undefined;
    previousQueueDescriptor = undefined;
    previousTargetDescriptor = undefined;
    createdGlobal = false;
    forwarding = false;
    originalQueue = undefined;
    originalQueueLength = 0;

    if (global && queue) {
      try {
        if (Object.getOwnPropertyDescriptor(global, 'que')?.value === queue) {
          if (shouldReturnPending && restoreQueue) {
            const later = ownQueueValues(queue).slice(restoreQueueLength);
            Array.prototype.push.apply(restoreQueue, later);
          }
          if (queueDescriptor) Object.defineProperty(global, 'que', queueDescriptor);
          else Reflect.deleteProperty(global, 'que');
        }
      } catch {
        // Publisher replacement wins over cleanup.
      }
    }
    if (removeGlobal) {
      try {
        if (
          Object.getOwnPropertyDescriptor(dependencies.target, 'testlight')?.value === global &&
          global &&
          Reflect.ownKeys(global).length === 0
        ) {
          if (targetDescriptor) {
            Object.defineProperty(dependencies.target, 'testlight', targetDescriptor);
          } else {
            Reflect.deleteProperty(dependencies.target, 'testlight');
          }
        }
      } catch {
        // Publisher replacement wins over cleanup.
      }
    }
  };

  return Object.freeze({
    activate: (_config: unknown): (() => void) => {
      if (active) throw new Error('Testlight runtime is already active');
      try {
        previousTargetDescriptor = Object.getOwnPropertyDescriptor(
          dependencies.target,
          'testlight'
        );
        if (previousTargetDescriptor && !('value' in previousTargetDescriptor)) {
          throw new TypeError('Testlight publisher global accessor is unsupported');
        }
        const currentGlobal = previousTargetDescriptor?.value;
        const global =
          typeof currentGlobal === 'object' && currentGlobal !== null ? currentGlobal : {};
        createdGlobal = global !== currentGlobal;
        if (createdGlobal) dependencies.target.testlight = global;
        ownedGlobal = global;

        previousQueueDescriptor = Object.getOwnPropertyDescriptor(global, 'que');
        if (previousQueueDescriptor && !('value' in previousQueueDescriptor)) {
          throw new TypeError('Testlight publisher queue accessor is unsupported');
        }
        originalQueue =
          previousQueueDescriptor &&
          'value' in previousQueueDescriptor &&
          Array.isArray(previousQueueDescriptor.value)
            ? previousQueueDescriptor.value
            : undefined;
        const queue = ownQueueValues(originalQueue);
        originalQueueLength = queue.length;
        Object.defineProperty(global, 'que', {
          configurable: true,
          enumerable: true,
          value: queue,
          writable: true,
        });
        installedQueue = queue;
        forwarding = false;
        active = true;
        started = false;
      } catch (error) {
        releaseOwnership();
        throw error;
      }
      return (): void => {
        if (!active) return;
        active = false;
        started = false;
        releaseOwnership();
      };
    },
    start: (_config: unknown): void => {
      if (!active || started) return;
      started = true;
      dependencies.started();

      try {
        const queue = installedQueue;
        if (
          !queue ||
          !ownedGlobal ||
          Object.getOwnPropertyDescriptor(ownedGlobal, 'que')?.value !== queue
        ) {
          return;
        }
        const pending = ownQueueValues(queue);
        queue.length = 0;
        Object.defineProperty(queue, 'push', {
          configurable: true,
          enumerable: false,
          value: (...candidates: unknown[]): number => {
            for (const candidate of candidates) {
              if (typeof candidate !== 'function') continue;
              try {
                dependencies.enqueue(candidate as () => void);
                log.debug('testlight shim: flushed callback');
              } catch (error) {
                log.debug('testlight shim: queued callback threw', error);
              }
            }
            return 0;
          },
          writable: false,
        });
        forwarding = true;
        for (const candidate of pending) queue.push(candidate);
      } catch (error) {
        releaseOwnership();
        throw error;
      }
    },
  });
}

export function createTestlightIntegrationRegistration(release: string): IntegrationRegistration {
  return createLifecycleIntegrationRegistration(TESTLIGHT_INTEGRATION_ID, release, {
    validateConfig: (candidate) => candidate === undefined,
  });
}
