// Sole production bootstrap for the resilient TSJS runtime.
declare const __TSJS_RUNTIME_CLAIMED_V1__: unknown;

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
import {
  coordinatePreparedFirstDisplayTakeoverV1,
  finalizeFirstDisplayAgentCaptureV1,
} from '../shared/first_display_handoff';
import type { ClaimedFirstDisplayTakeoverV1 } from '../shared/takeover';

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
  readonly source: object;
  readonly target: BootstrapTarget;
  readonly boot: Readonly<TsjsBootV1>;
  readonly integrity: Readonly<ServerBootIntegrityV1>;
  readonly complete: (outcome: 'kernel' | 'abi_mismatch' | 'bundle_partial') => void;
  readonly currentScript: () => HTMLScriptElement | null;
  readonly mode: 'direct' | 'takeover';
  readonly bind: (input: unknown) => unknown;
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

export type BrowserRuntimeCompositionFactory = (
  runtimeOptions: RuntimeOptions,
  compositionOptions: Readonly<Record<string, never>>
) => Readonly<{ runtime: Runtime }>;

function claimedRuntimeCandidate(): unknown {
  if (typeof __TSJS_RUNTIME_CLAIMED_V1__ !== 'undefined') {
    return __TSJS_RUNTIME_CLAIMED_V1__;
  }
  try {
    const source = (window as unknown as { tsjs?: unknown }).tsjs;
    if ((typeof source !== 'object' && typeof source !== 'function') || source === null) return;
    const claim = (source as { _claimRuntimeV1?: unknown })._claimRuntimeV1;
    return typeof claim === 'function' ? claim(source) : undefined;
  } catch {
    return;
  }
}

function retainClaimedServerBootV1(candidate: unknown): ClaimedServerBootV1 | undefined {
  try {
    const fields = ownDataObject(candidate, [
      'boot',
      'integrity',
      'complete',
      'currentScript',
      'source',
      'target',
      'mode',
      'bind',
    ]);
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
      typeof fields['complete'] !== 'function' ||
      typeof fields['currentScript'] !== 'function' ||
      (typeof fields['source'] !== 'object' && typeof fields['source'] !== 'function') ||
      fields['source'] === null ||
      (typeof fields['target'] !== 'object' && typeof fields['target'] !== 'function') ||
      fields['target'] === null ||
      (fields['mode'] !== 'direct' && fields['mode'] !== 'takeover') ||
      typeof fields['bind'] !== 'function'
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
  const fields = ownDataObject(candidate, [
    'boot',
    'integrity',
    'complete',
    'currentScript',
    'source',
    'target',
    'mode',
    'bind',
  ]);
  return fields && typeof fields['complete'] === 'function'
    ? (fields['complete'] as ClaimedServerBootV1['complete'])
    : undefined;
}

/** Claim the browser namespace and start the injected sole composition root. */
export function startProductionRuntime(createComposition: BrowserRuntimeCompositionFactory): void {
  const candidate = claimedRuntimeCandidate();
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
  const source = claimed.source;
  const target = claimed.target;
  let authenticatedSource: HTMLScriptElement | null;
  try {
    authenticatedSource = claimed.currentScript();
  } catch {
    authenticatedSource = null;
  }
  if (authenticatedSource !== source) {
    try {
      claimed.complete('abi_mismatch');
    } catch {
      // The authenticated bootstrap watchdog remains the terminal fallback owner.
    }
    return;
  }
  const { boot } = claimed;
  const directMode = boot.manifest.firstDisplay === null;
  if ((directMode && claimed.mode !== 'direct') || (!directMode && claimed.mode !== 'takeover')) {
    try {
      claimed.complete('abi_mismatch');
    } catch {
      // The authenticated bootstrap watchdog remains the terminal fallback owner.
    }
    return;
  }
  let trustedScriptUrl: ((value: string) => unknown) | undefined;
  let trustedCurrentScript: (() => HTMLScriptElement | null) | undefined = claimed.currentScript;
  const composition = createComposition(
    {
      target,
      releaseId: EMBEDDED_RELEASE_ID,
      manifest: boot.manifest,
      knownIntegrationIds: EMBEDDED_INTEGRATION_IDS,
      catalog: EMBEDDED_RUNTIME_CATALOG,
      boot,
      currentScript: () => trustedCurrentScript?.() ?? null,
      ...(!directMode
        ? {
            trustedScriptUrl: (value: string) => {
              if (!trustedScriptUrl) throw new TypeError('tsjs');
              return trustedScriptUrl(value);
            },
            coordinateTakeover: (prepared) => {
              const leased = claimed.bind(finalizeFirstDisplayAgentCaptureV1) as
                ClaimedFirstDisplayTakeoverV1 | undefined;
              if (!leased) throw new TypeError('tsjs');
              trustedScriptUrl = leased[9];
              trustedCurrentScript = leased[10];
              if (
                !coordinatePreparedFirstDisplayTakeoverV1({
                  prepared,
                  finalized: leased[0],
                  outline: leased[1],
                  boot,
                  isCurrentGeneration: leased[2],
                  authenticateRuntimeScript: leased[3],
                  currentMutationRevision: leased[4],
                  quiesceAgent: () => undefined,
                  detachCommittedArtifacts: () => {
                    if (!leased[5]()) throw new TypeError('tsjs');
                  },
                  disposeAgent: leased[6],
                  onFailure: leased[7],
                })
              ) {
                throw new TypeError('tsjs');
              }
              leased[8]();
            },
          }
        : {}),
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
  const completeDirect = directMode
    ? (claimed.bind(() => composition.runtime.dispose()) as DirectRuntimeCompletion | undefined)
    : undefined;
  if (directMode && !completeDirect) {
    try {
      composition.runtime.dispose();
    } catch {
      // Completion still commits the independently authenticated terminal fallback.
    }
    try {
      claimed.complete('abi_mismatch');
    } catch {
      // The authenticated bootstrap watchdog remains the terminal fallback owner.
    }
    return;
  }
  if (!composition.runtime.start()) {
    completeDirect?.('failed_start');
  }
}
