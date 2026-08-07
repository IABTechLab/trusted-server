import type {
  IntegrationActivationContext,
  IntegrationPrepareContext,
  IntegrationRegistration,
} from '../../kernel/integration_registry';

import { installGptGuard, resetGuardState } from './script_guard';

export const GPT_INTEGRATION_ID = 'gpt' as const;

const MAX_CONFIG_DEPTH = 16;
const MAX_CONFIG_NODES = 512;
const MAX_CONFIG_MEMBERS = 256;
const arrayIsArrayIntrinsic = Array.isArray;
const numberIsFiniteIntrinsic = Number.isFinite;
const objectGetOwnPropertyDescriptorIntrinsic = Object.getOwnPropertyDescriptor;
const objectGetOwnPropertyNamesIntrinsic = Object.getOwnPropertyNames;
const objectGetOwnPropertySymbolsIntrinsic = Object.getOwnPropertySymbols;
const objectGetPrototypeOfIntrinsic = Object.getPrototypeOf;
const objectIsFrozenIntrinsic = Object.isFrozen;

interface GptIntegrationRuntime {
  readonly start: (config: unknown) => void;
}

function validFrozenConfig(candidate: unknown): boolean {
  const seen = new Set<object>();
  let nodes = 0;
  const visit = (value: unknown, depth: number, topLevel: boolean): boolean => {
    if (value === undefined) return topLevel;
    if (value === null || typeof value === 'string' || typeof value === 'boolean') return true;
    if (typeof value === 'number') return numberIsFiniteIntrinsic(value);
    if (typeof value !== 'object' || depth > MAX_CONFIG_DEPTH || nodes >= MAX_CONFIG_NODES) {
      return false;
    }
    if (seen.has(value) || !objectIsFrozenIntrinsic(value)) return false;
    seen.add(value);
    nodes += 1;

    const array = arrayIsArrayIntrinsic(value);
    const prototype = objectGetPrototypeOfIntrinsic(value);
    if (
      (!array && prototype !== Object.prototype && prototype !== null) ||
      (array && prototype !== Array.prototype)
    ) {
      return false;
    }
    if (objectGetOwnPropertySymbolsIntrinsic(value).length !== 0) return false;
    const names = objectGetOwnPropertyNamesIntrinsic(value);
    if (names.length > MAX_CONFIG_MEMBERS + (array ? 1 : 0)) return false;
    if (array) {
      const length = objectGetOwnPropertyDescriptorIntrinsic(value, 'length');
      if (!length || !('value' in length) || names.length !== length.value + 1) return false;
      for (let index = 0; index < length.value; index += 1) {
        const descriptor = objectGetOwnPropertyDescriptorIntrinsic(value, String(index));
        if (!descriptor || !descriptor.enumerable || !('value' in descriptor)) return false;
        if (!visit(descriptor.value, depth + 1, false)) return false;
      }
      return true;
    }

    for (const name of names) {
      const descriptor = objectGetOwnPropertyDescriptorIntrinsic(value, name);
      if (!descriptor || !descriptor.enumerable || !('value' in descriptor)) return false;
      if (!visit(descriptor.value, depth + 1, false)) return false;
    }
    return true;
  };

  try {
    return visit(candidate, 0, true);
  } catch {
    return false;
  }
}

function readGptRuntime(
  interfaces: Readonly<Record<string, unknown>>
): GptIntegrationRuntime | undefined {
  const descriptor = Object.getOwnPropertyDescriptor(interfaces, GPT_INTEGRATION_ID);
  if (!descriptor || !('value' in descriptor)) return undefined;
  const candidate = descriptor.value;
  if (
    typeof candidate !== 'object' ||
    candidate === null ||
    Array.isArray(candidate) ||
    !Object.isFrozen(candidate) ||
    Reflect.ownKeys(candidate).length !== 1
  ) {
    return undefined;
  }
  const start = Object.getOwnPropertyDescriptor(candidate, 'start');
  if (!start || !('value' in start) || typeof start.value !== 'function') return undefined;
  return candidate as GptIntegrationRuntime;
}

/** Build the release-bound GPT module registered by the coordinated runtime. */
export function createGptIntegrationRegistration(release: string): IntegrationRegistration {
  return Object.freeze({
    id: GPT_INTEGRATION_ID,
    release,
    prepare: ({ config, interfaces }: IntegrationPrepareContext) => {
      if (!validFrozenConfig(config)) throw new TypeError('GPT integration config is invalid');
      const runtime = readGptRuntime(interfaces);
      if (!runtime) throw new TypeError('GPT integration runtime is unavailable');

      return Object.freeze({
        activate: ({ afterCommit, onDispose }: IntegrationActivationContext) => {
          // Register restoration before the first live browser mutation.
          onDispose(resetGuardState);
          installGptGuard();
          afterCommit(() => runtime.start(config));
        },
      });
    },
  });
}
