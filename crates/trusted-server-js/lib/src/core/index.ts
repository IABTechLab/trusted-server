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
import { validateRuntimeManifestV1 } from '../kernel/integration_registry';
import { consumeFirstDisplayTakeoverTransport } from '../shared/takeover';

import { ownDataObject } from './contracts/auction_projection';
import { snapshotFrozenTsjsBootV1 } from './contracts/boot';
import type { ServerBootIntegrityV1 } from './contracts/server_boot_transport';
import { sha256HexUtf8V1 } from './contracts/sha256';
import { EMBEDDED_INTEGRATION_IDS, EMBEDDED_RELEASE_ID, EMBEDDED_RUNTIME_CATALOG } from './release';
import type { TsjsBootV1 } from './types';

type BootstrapTarget = object & {
  boot?: unknown;
  que?: unknown;
};

type DirectRuntimeCompletion = (outcome: 'kernel' | 'runtime_fallback' | 'failed_start') => void;

interface ClaimedServerBootV1 {
  readonly boot: Readonly<TsjsBootV1>;
  readonly integrity: Readonly<ServerBootIntegrityV1>;
  readonly complete: (outcome: 'kernel' | 'abi_mismatch' | 'bundle_partial') => void;
}

function retainServerBoot(candidate: unknown, releaseId: string): Readonly<TsjsBootV1> | undefined {
  try {
    const boot = snapshotFrozenTsjsBootV1(candidate, releaseId);
    if (!boot) return undefined;
    const manifest = validateRuntimeManifestV1(
      boot.manifest,
      releaseId,
      EMBEDDED_RUNTIME_CATALOG,
      true
    );
    return manifest && JSON.stringify(boot.manifest) === JSON.stringify(manifest)
      ? boot
      : undefined;
  } catch {
    return undefined;
  }
}

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
): ((source: unknown) => Readonly<ClaimedServerBootV1> | undefined) | null | undefined {
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
    return descriptor.value as (source: unknown) => Readonly<ClaimedServerBootV1> | undefined;
  } catch {
    return null;
  }
}

function retainClaimedServerBootV1(candidate: unknown): ClaimedServerBootV1 | undefined {
  try {
    const fields = ownDataObject(candidate, ['boot', 'integrity', 'complete']);
    if (!fields || !Object.isFrozen(candidate)) return undefined;
    const boot = retainServerBoot(fields['boot'], EMBEDDED_RELEASE_ID);
    const integrity = fields['integrity'];
    const acceptedIntegrity = ownDataObject(integrity, [
      'version',
      'projectionDigest',
      'integrationConfigDigest',
    ]);
    if (
      !boot ||
      !acceptedIntegrity ||
      !Object.isFrozen(integrity) ||
      typeof fields['complete'] !== 'function'
    ) {
      return undefined;
    }
    if (
      acceptedIntegrity['version'] !== 1 ||
      acceptedIntegrity['projectionDigest'] !==
        sha256HexUtf8V1(JSON.stringify(boot.auctionProjection)) ||
      acceptedIntegrity['integrationConfigDigest'] !==
        sha256HexUtf8V1(JSON.stringify(boot.integrations))
    ) {
      return undefined;
    }
    return candidate as ClaimedServerBootV1;
  } catch {
    return undefined;
  }
}

function claimedBootCompletion(candidate: unknown): ClaimedServerBootV1['complete'] | undefined {
  const fields = ownDataObject(candidate, ['boot', 'integrity', 'complete']);
  return fields && typeof fields['complete'] === 'function'
    ? (fields['complete'] as ClaimedServerBootV1['complete'])
    : undefined;
}

/** Claim the browser namespace and start the injected sole composition root. */
export function startProductionRuntime(createComposition: BrowserRuntimeCompositionFactory): void {
  const target = bootstrapTarget();
  if (!target) return;
  const claimBoot = bootSnapshotClaim(target);
  if (!claimBoot) return;
  const source = document.currentScript;
  const candidate = claimBoot(source);
  const completeBoot = claimedBootCompletion(candidate);
  const claimed = retainClaimedServerBootV1(candidate);
  if (!claimed) {
    try {
      completeBoot?.('abi_mismatch');
    } catch {
      // The authenticated bootstrap watchdog remains the terminal fallback owner.
    }
    return;
  }
  const { boot } = claimed;
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
        try {
          claimed.complete(result.state === 'kernel' ? 'kernel' : result.reason);
        } catch {
          // Direct completion still closes its independently authenticated watchdog.
        }
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
