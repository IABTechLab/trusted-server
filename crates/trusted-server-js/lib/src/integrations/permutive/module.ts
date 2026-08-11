import type { IntegrationRegistration } from '../../kernel/integration_registry';
import {
  createLifecycleIntegrationRegistration,
  type IntegrationLifecycleRuntime,
} from '../../kernel/lifecycle_module';
import { log } from '../../core/log';

import { installPermutiveGuard, resetGuardState } from './script_guard';
import { getPermutiveSegments } from './segments';

export const PERMUTIVE_INTEGRATION_ID = 'permutive_context' as const;

const PERMUTIVE_CONFIG_FIELDS = [
  'apiHost',
  'apiProtocol',
  'cdnBaseUrl',
  'cdnProtocol',
  'secureSignalsApiHost',
  'segmentSyncApiHost',
] as const;

type PermutiveConfigField = (typeof PERMUTIVE_CONFIG_FIELDS)[number];
type PermutiveConfig = Record<PermutiveConfigField, string>;

interface PermutiveSdk {
  readonly config: PermutiveConfig;
}

type ContextContributor = () => Readonly<Record<string, unknown>> | undefined;

export interface PermutiveRuntimeDependencies {
  readonly clearTimeout: (timer: number) => void;
  readonly getSdk: () => PermutiveSdk | undefined;
  readonly getSegments: () => readonly string[];
  readonly installGuard: () => void;
  readonly location: { readonly host: string; readonly protocol: string };
  readonly registerContext: (contributor: ContextContributor) => (() => void) | undefined;
  readonly resetGuard: () => void;
  readonly setTimeout: (callback: () => void, delay: number) => number;
  readonly started: () => void;
  readonly timedOut: () => void;
}

function bestEffort(action: () => void): void {
  try {
    action();
  } catch {
    // Cleanup is intentionally isolated so one failed release cannot retain another resource.
  }
}

function snapshotSegments(candidate: readonly string[]): readonly string[] {
  const segments: string[] = [];
  try {
    const length = Math.min(candidate.length, 100);
    for (let index = 0; index < length; index += 1) {
      const segment = candidate[index];
      if (typeof segment === 'string') segments.push(segment);
    }
  } catch {
    return Object.freeze([]);
  }
  return Object.freeze(segments);
}

/** Own the Permutive guard, auction context, SDK readiness timer, and rewritten config. */
export function createPermutiveRuntime(
  overrides: Partial<PermutiveRuntimeDependencies> = {}
): IntegrationLifecycleRuntime {
  const dependencies: PermutiveRuntimeDependencies = {
    clearTimeout: (timer) => window.clearTimeout(timer),
    getSdk: () => (globalThis as typeof globalThis & { permutive?: PermutiveSdk }).permutive,
    getSegments: getPermutiveSegments,
    installGuard: installPermutiveGuard,
    location: window.location,
    registerContext: () => undefined,
    resetGuard: resetGuardState,
    setTimeout: (callback, delay) => window.setTimeout(callback, delay),
    started: () => log.info('Permutive integration initialized'),
    timedOut: () => log.warn('Permutive SDK not detected after', 2_500, 'ms'),
    ...overrides,
  };
  let active = false;
  let started = false;
  let timer: number | undefined;
  let releaseContext: (() => void) | undefined;
  let ownedConfig: PermutiveConfig | undefined;
  let installedValues: Readonly<PermutiveConfig> | undefined;
  let previousValues: Readonly<PermutiveConfig> | undefined;

  const resetSdk = (): void => {
    const config = ownedConfig;
    const installed = installedValues;
    const previous = previousValues;
    ownedConfig = undefined;
    installedValues = undefined;
    previousValues = undefined;
    if (!config || !installed || !previous) return;

    for (const field of PERMUTIVE_CONFIG_FIELDS) {
      bestEffort(() => {
        if (config[field] === installed[field]) config[field] = previous[field];
      });
    }
  };

  const installSdkConfig = (config: PermutiveConfig): boolean => {
    const protocol = dependencies.location.protocol === 'https:' ? 'https' : 'http';
    const host = dependencies.location.host;
    const next: PermutiveConfig = {
      apiHost: `${host}/integrations/permutive/api`,
      apiProtocol: protocol,
      cdnBaseUrl: `${host}/integrations/permutive/cdn`,
      cdnProtocol: protocol,
      secureSignalsApiHost: `${host}/integrations/permutive/secure-signal`,
      segmentSyncApiHost: `${host}/integrations/permutive/sync`,
    };
    const previous = {} as PermutiveConfig;
    const written: PermutiveConfigField[] = [];
    try {
      for (const field of PERMUTIVE_CONFIG_FIELDS) previous[field] = config[field];
      for (const field of PERMUTIVE_CONFIG_FIELDS) {
        config[field] = next[field];
        written.push(field);
      }
    } catch {
      for (const field of written.reverse()) {
        bestEffort(() => {
          if (config[field] === next[field]) config[field] = previous[field];
        });
      }
      return false;
    }
    ownedConfig = config;
    previousValues = Object.freeze({ ...previous });
    installedValues = Object.freeze({ ...next });
    return true;
  };

  return Object.freeze({
    activate: (_config: unknown): (() => void) => {
      if (active) throw new Error('Permutive runtime is already active');
      try {
        dependencies.installGuard();
        releaseContext = dependencies.registerContext(() => {
          try {
            const segments = snapshotSegments(dependencies.getSegments());
            if (segments.length === 0) return undefined;
            return Object.freeze({ permutive_segments: segments });
          } catch {
            return undefined;
          }
        });
        if (!releaseContext) throw new Error('Permutive context registration failed');
      } catch (error) {
        releaseContext = undefined;
        bestEffort(dependencies.resetGuard);
        throw error;
      }
      active = true;
      return (): void => {
        if (!active) return;
        active = false;
        started = false;
        if (timer !== undefined) {
          bestEffort(() => dependencies.clearTimeout(timer as number));
          timer = undefined;
        }
        resetSdk();
        const release = releaseContext;
        releaseContext = undefined;
        if (release) bestEffort(release);
        bestEffort(dependencies.resetGuard);
      };
    },
    start: (_config: unknown): void => {
      if (!active || started) return;
      started = true;
      dependencies.started();
      let attempts = 0;
      const check = (): void => {
        timer = undefined;
        if (!active) return;
        attempts += 1;
        try {
          const sdk = dependencies.getSdk();
          if (sdk?.config && installSdkConfig(sdk.config)) return;
        } catch {
          // Treat an unreadable SDK as not ready.
        }
        if (attempts >= 50) {
          dependencies.timedOut();
          return;
        }
        try {
          timer = dependencies.setTimeout(check, 50);
        } catch {
          dependencies.timedOut();
        }
      };
      check();
    },
  });
}

export function createPermutiveIntegrationRegistration(release: string): IntegrationRegistration {
  return createLifecycleIntegrationRegistration(PERMUTIVE_INTEGRATION_ID, release, {
    validateConfig: (candidate) => candidate === undefined,
  });
}
