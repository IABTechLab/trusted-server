// Sole production bootstrap for the resilient TSJS runtime.
export type {
  AddAdUnitsResult,
  AdUnit,
  GptDiagnosticsApi,
  GptDiagnosticsExportV1,
  GptDiagnosticsRequestCycle,
  ProgrammaticAdUnit,
  RequestAdsOptions,
  RequestAdsResult,
  TsjsApi,
  TsjsBootV1,
  TsjsDiagnostics,
} from './types';
export type { Runtime, RuntimeOptions, RuntimeState } from '../kernel/runtime';

import type { Runtime, RuntimeOptions } from '../kernel/runtime';

import { EMBEDDED_INTEGRATION_IDS, EMBEDDED_RELEASE_ID } from './release';

const KNOWN_INTEGRATIONS = new Set(EMBEDDED_INTEGRATION_IDS);
const MAX_CONFIG_DEPTH = 16;
const MAX_CONFIG_NODES = 512;
const MAX_CONFIG_MEMBERS = 256;
const INVALID_CONFIG = Symbol('invalid-config');

type BootstrapTarget = object & {
  boot?: unknown;
  que?: unknown;
  _integrationConfig?: unknown;
};

export type BrowserRuntimeCompositionFactory = (
  runtimeOptions: RuntimeOptions,
  compositionOptions: Readonly<Record<string, never>>
) => Readonly<{ runtime: Runtime }>;

function bootstrapTarget(): BootstrapTarget | undefined {
  try {
    const current = (window as unknown as { tsjs?: unknown }).tsjs;
    if ((typeof current === 'object' || typeof current === 'function') && current !== null) {
      return current as BootstrapTarget;
    }
    const target: BootstrapTarget = {};
    (window as unknown as { tsjs?: unknown }).tsjs = target;
    return target;
  } catch {
    return undefined;
  }
}

function snapshotConfigValue(
  candidate: unknown,
  seen: Set<object>,
  state: { nodes: number },
  depth = 0
): unknown | typeof INVALID_CONFIG {
  if (candidate === null || typeof candidate === 'string' || typeof candidate === 'boolean') {
    return candidate;
  }
  if (typeof candidate === 'number') {
    return Number.isFinite(candidate) ? candidate : INVALID_CONFIG;
  }
  if (typeof candidate !== 'object' || depth > MAX_CONFIG_DEPTH || seen.has(candidate)) {
    return INVALID_CONFIG;
  }
  if (state.nodes >= MAX_CONFIG_NODES) return INVALID_CONFIG;
  seen.add(candidate);
  state.nodes += 1;
  try {
    const isArray = Array.isArray(candidate);
    const prototype = Object.getPrototypeOf(candidate) as unknown;
    if (
      (isArray && prototype !== Array.prototype) ||
      (!isArray && prototype !== Object.prototype && prototype !== null) ||
      Object.getOwnPropertySymbols(candidate).length !== 0
    ) {
      return INVALID_CONFIG;
    }
    const names = Object.getOwnPropertyNames(candidate);
    if (names.length > MAX_CONFIG_MEMBERS + (isArray ? 1 : 0)) return INVALID_CONFIG;
    if (isArray) {
      const length = Object.getOwnPropertyDescriptor(candidate, 'length');
      if (!length || !('value' in length) || names.length !== length.value + 1) {
        return INVALID_CONFIG;
      }
      const values: unknown[] = [];
      for (let index = 0; index < length.value; index += 1) {
        const descriptor = Object.getOwnPropertyDescriptor(candidate, String(index));
        if (!descriptor || !descriptor.enumerable || !('value' in descriptor)) {
          return INVALID_CONFIG;
        }
        const value = snapshotConfigValue(descriptor.value, seen, state, depth + 1);
        if (value === INVALID_CONFIG) return INVALID_CONFIG;
        values.push(value);
      }
      return Object.freeze(values);
    }
    const copy: Record<string, unknown> = {};
    for (const name of names) {
      const descriptor = Object.getOwnPropertyDescriptor(candidate, name);
      if (!descriptor || !descriptor.enumerable || !('value' in descriptor)) {
        return INVALID_CONFIG;
      }
      const value = snapshotConfigValue(descriptor.value, seen, state, depth + 1);
      if (value === INVALID_CONFIG) return INVALID_CONFIG;
      copy[name] = value;
    }
    return Object.freeze(copy);
  } catch {
    return INVALID_CONFIG;
  }
}

function snapshotIntegrationConfig(
  candidate: unknown
): Readonly<Record<string, unknown>> | undefined {
  try {
    if (
      typeof candidate !== 'object' ||
      candidate === null ||
      Array.isArray(candidate) ||
      (Object.getPrototypeOf(candidate) !== Object.prototype &&
        Object.getPrototypeOf(candidate) !== null) ||
      Object.getOwnPropertySymbols(candidate).length !== 0
    ) {
      return undefined;
    }
    const names = Object.getOwnPropertyNames(candidate);
    if (names.length > EMBEDDED_INTEGRATION_IDS.length) return undefined;
    const configs: Record<string, unknown> = {};
    const seen = new Set<object>();
    const state = { nodes: 0 };
    for (const name of names) {
      if (!KNOWN_INTEGRATIONS.has(name)) return undefined;
      const configDescriptor = Object.getOwnPropertyDescriptor(candidate, name);
      if (!configDescriptor || !configDescriptor.enumerable || !('value' in configDescriptor)) {
        return undefined;
      }
      const value = snapshotConfigValue(configDescriptor.value, seen, state);
      if (value === INVALID_CONFIG) return undefined;
      configs[name] = value;
    }
    return Object.freeze(configs);
  } catch {
    return undefined;
  }
}

function consumeIntegrationConfig(
  target: BootstrapTarget
): Readonly<Record<string, unknown>> | undefined {
  let descriptor: PropertyDescriptor | undefined;
  try {
    descriptor = Object.getOwnPropertyDescriptor(target, '_integrationConfig');
  } catch {
    return undefined;
  }
  if (!descriptor) return Object.freeze({});
  if (!('value' in descriptor) || !descriptor.configurable) return undefined;

  const configs = snapshotIntegrationConfig(descriptor.value);
  try {
    if (!Reflect.deleteProperty(target, '_integrationConfig')) return undefined;
  } catch {
    return undefined;
  }
  return configs;
}

function bootManifest(target: BootstrapTarget): unknown {
  try {
    const boot = Object.getOwnPropertyDescriptor(target, 'boot');
    if (!boot || !('value' in boot) || typeof boot.value !== 'object' || boot.value === null) {
      return undefined;
    }
    const manifest = Object.getOwnPropertyDescriptor(boot.value, 'manifest');
    return manifest && 'value' in manifest ? manifest.value : undefined;
  } catch {
    return undefined;
  }
}

/** Claim the browser namespace and start the injected sole composition root. */
export function startProductionRuntime(createComposition: BrowserRuntimeCompositionFactory): void {
  const target = bootstrapTarget();
  if (!target) return;
  const configs = consumeIntegrationConfig(target);
  const composition = createComposition(
    {
      target,
      releaseId: EMBEDDED_RELEASE_ID,
      manifest: configs ? bootManifest(target) : undefined,
      knownIntegrationIds: EMBEDDED_INTEGRATION_IDS,
      getBindings: (id) =>
        Object.freeze({
          config: configs?.[id],
          interfaces: Object.freeze({}),
        }),
      kernel: {
        addAdUnits: () => Object.freeze({ registered: Object.freeze([]) }),
        diagnostics: Object.freeze({}),
        requestAds: async () => Object.freeze({ slots: Object.freeze([]) }),
      },
    },
    {}
  );
  if (!composition.runtime.start()) return;

  let requested = false;
  const install = (): void => {
    if (requested) return;
    requested = true;
    void composition.runtime.install();
  };
  queueMicrotask(install);
}
