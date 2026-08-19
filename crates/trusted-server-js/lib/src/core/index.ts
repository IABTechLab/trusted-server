// Sole production bootstrap for the resilient TSJS runtime.
export type {
  AddAdUnitsResult,
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
import { consumeFirstDisplayTakeoverTransport } from '../shared/takeover';

import type { TsjsBootV1 } from './types';
import { EMBEDDED_INTEGRATION_IDS, EMBEDDED_RELEASE_ID, EMBEDDED_RUNTIME_CATALOG } from './release';

type BootstrapTarget = object & {
  boot?: unknown;
  que?: unknown;
};

type DirectRuntimeCompletion = (outcome: 'kernel' | 'runtime_fallback' | 'failed_start') => void;

function directRuntimeClaim(
  target: BootstrapTarget
): ((source: unknown, cancel: unknown) => DirectRuntimeCompletion | undefined) | null | undefined {
  try {
    const descriptor = Object.getOwnPropertyDescriptor(target, '_claimDirectRuntime');
    if (!descriptor) return undefined;
    if (
      !descriptor.configurable ||
      descriptor.enumerable ||
      descriptor.writable ||
      !('value' in descriptor) ||
      typeof descriptor.value !== 'function'
    ) {
      return null;
    }
    return descriptor.value as (
      source: unknown,
      cancel: unknown
    ) => DirectRuntimeCompletion | undefined;
  } catch {
    return null;
  }
}

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

function bootSnapshotClaim(
  target: BootstrapTarget
): ((source: unknown) => Readonly<TsjsBootV1> | undefined) | null | undefined {
  try {
    const descriptor = Object.getOwnPropertyDescriptor(target, '_claimBootSnapshot');
    if (!descriptor) return undefined;
    if (
      !descriptor.configurable ||
      descriptor.enumerable ||
      descriptor.writable ||
      !('value' in descriptor) ||
      typeof descriptor.value !== 'function'
    ) {
      return null;
    }
    return descriptor.value as (source: unknown) => Readonly<TsjsBootV1> | undefined;
  } catch {
    return null;
  }
}

/** Claim the browser namespace and start the injected sole composition root. */
export function startProductionRuntime(createComposition: BrowserRuntimeCompositionFactory): void {
  const target = bootstrapTarget();
  if (!target) return;
  const claimBoot = bootSnapshotClaim(target);
  if (!claimBoot) return;
  const boot = claimBoot(document.currentScript);
  if (!boot) return;
  const takeover = consumeFirstDisplayTakeoverTransport(target);
  if (takeover.status === 'invalid') return;
  const claimDirect = takeover.status === 'absent' ? directRuntimeClaim(target) : undefined;
  if (claimDirect === null) return;
  const composition = createComposition(
    {
      target,
      releaseId: EMBEDDED_RELEASE_ID,
      manifest: boot.manifest,
      knownIntegrationIds: EMBEDDED_INTEGRATION_IDS,
      catalog: EMBEDDED_RUNTIME_CATALOG,
      boot,
      ...(takeover.status === 'accepted' ? { coordinateTakeover: takeover.coordinate } : {}),
      autoInstall: true,
      onInstallComplete: (result) => {
        completeDirect?.(result.state === 'kernel' ? 'kernel' : 'runtime_fallback');
      },
      kernel: {
        addAdUnits: () => Object.freeze({ registered: Object.freeze([]) }),
        diagnostics: Object.freeze({}),
        requestAds: async () => Object.freeze({ slots: Object.freeze([]) }),
      },
    },
    {}
  );
  const completeDirect = claimDirect?.(document.currentScript, () => composition.runtime.dispose());
  if (claimDirect && !completeDirect) return;
  if (!composition.runtime.start()) {
    completeDirect?.('failed_start');
  }
}
