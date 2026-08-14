import {
  type PersistentFirstDisplaySliceStateV1,
  validatePersistentFirstDisplaySliceAdoptionV1,
} from '../shared/takeover';

import type {
  IntegrationActivationContext,
  IntegrationPrepareContext,
  IntegrationRegistration,
} from './integration_registry';
import { RELEASE_CATALOG } from './release_catalog';

const MAX_CONFIG_DEPTH = 16;
const MAX_CONFIG_NODES = 512;
const MAX_CONFIG_MEMBERS = 256;

export interface IntegrationLifecycleRuntime {
  readonly activate: (config: unknown) => () => void;
  readonly start: (config: unknown) => void;
}

export interface LifecycleIntegrationRegistrationOptions {
  readonly createOwnedRuntime?: (context: IntegrationPrepareContext) => IntegrationLifecycleRuntime;
  readonly firstDisplaySliceId?: string;
  readonly validateConfig?: (candidate: unknown) => boolean;
  readonly validateFirstDisplayState?: (
    candidate: Readonly<PersistentFirstDisplaySliceStateV1>
  ) => boolean;
}

function validFrozenConfig(candidate: unknown): boolean {
  if (
    typeof candidate !== 'object' ||
    candidate === null ||
    Array.isArray(candidate) ||
    !Object.isFrozen(candidate) ||
    Object.getPrototypeOf(candidate) !== Object.prototype
  ) {
    return false;
  }
  const visited = new Set<object>();
  let nodes = 0;
  const visit = (value: unknown, depth: number): boolean => {
    if (value === null || (typeof value !== 'object' && typeof value !== 'function')) return true;
    if (typeof value === 'function' || depth > MAX_CONFIG_DEPTH || visited.has(value)) return false;
    if (nodes >= MAX_CONFIG_NODES || !Object.isFrozen(value)) return false;
    nodes += 1;
    visited.add(value);
    const isArray = Array.isArray(value);
    if (!isArray && Object.getPrototypeOf(value) !== Object.prototype) return false;
    const keys = Reflect.ownKeys(value);
    if (keys.length > MAX_CONFIG_MEMBERS + (isArray ? 1 : 0)) return false;
    for (let index = 0; index < keys.length; index += 1) {
      const key = keys[index];
      if (typeof key !== 'string') return false;
      if (isArray && key === 'length') continue;
      if (isArray && key !== String(index)) return false;
      const descriptor = Object.getOwnPropertyDescriptor(value, key);
      if (!descriptor || !descriptor.enumerable || !('value' in descriptor)) return false;
      if (!visit(descriptor.value, depth + 1)) return false;
    }
    return true;
  };
  try {
    return visit(candidate, 0);
  } catch {
    return false;
  }
}

function readRuntime(
  id: string,
  interfaces: Readonly<Record<string, unknown>>
): IntegrationLifecycleRuntime | undefined {
  try {
    const descriptor = Object.getOwnPropertyDescriptor(interfaces, id);
    if (!descriptor || !('value' in descriptor)) return undefined;
    const runtime = descriptor.value;
    if (
      typeof runtime !== 'object' ||
      runtime === null ||
      Array.isArray(runtime) ||
      !Object.isFrozen(runtime) ||
      Reflect.ownKeys(runtime).length !== 2
    ) {
      return undefined;
    }
    const activate = Object.getOwnPropertyDescriptor(runtime, 'activate');
    const start = Object.getOwnPropertyDescriptor(runtime, 'start');
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
    return runtime as IntegrationLifecycleRuntime;
  } catch {
    return undefined;
  }
}

/** Build a release-bound registration around one exact composition-owned runtime. */
export function createLifecycleIntegrationRegistration(
  id: string,
  releaseId: string,
  options: LifecycleIntegrationRegistrationOptions = {}
): IntegrationRegistration {
  const catalogEntry = RELEASE_CATALOG.find((entry) => entry.id === id);
  if (!catalogEntry) throw new TypeError(`Unknown release catalog module: ${id}`);
  return Object.freeze({
    abi: 1,
    id,
    phase: catalogEntry.phase,
    releaseId,
    prepare: async (context: IntegrationPrepareContext) => {
      const { config, interfaces } = context;
      const validateConfig = options.validateConfig ?? validFrozenConfig;
      let configValid: boolean;
      try {
        configValid = validateConfig(config);
      } catch {
        configValid = false;
      }
      if (!configValid) {
        throw new TypeError(`${id} integration config is invalid`);
      }
      const runtime = options.createOwnedRuntime?.(context) ?? readRuntime(id, interfaces);
      if (!runtime) throw new TypeError(`${id} integration runtime is unavailable`);

      return Object.freeze({
        activate: ({ adoption, afterCommit, onDispose }: IntegrationActivationContext) => {
          if (adoption !== undefined && options.firstDisplaySliceId !== undefined) {
            if (
              !validatePersistentFirstDisplaySliceAdoptionV1(
                adoption,
                options.firstDisplaySliceId,
                options.validateFirstDisplayState
              )
            ) {
              throw new TypeError(`${id} first-display parser state is invalid`);
            }
          }
          const runtimeRelease: { value?: () => void } = {};
          onDispose(() => runtimeRelease.value?.());
          const releaseRuntime = runtime.activate(config);
          if (typeof releaseRuntime !== 'function') {
            throw new TypeError(`${id} integration disposer is unavailable`);
          }
          runtimeRelease.value = releaseRuntime;
          afterCommit(() => runtime.start(config));
        },
      });
    },
  });
}
