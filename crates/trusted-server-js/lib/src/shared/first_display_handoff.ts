import type { BootFailureReason } from '../kernel/fallback_surface';
import type { PreparedKernelTakeover } from '../kernel/integration_registry';
import type { FirstDisplaySliceId } from '../kernel/release_catalog';
import type { FirstDisplayRenderCaptureV1 } from '../first_display/driver';
import type {
  FirstDisplayGptCaptureCycleV1,
  FirstDisplayGptDiagnosticsCaptureV1,
} from '../first_display/leaf/gpt_protocol';
import type { FirstDisplayProjectedKind } from '../first_display/leaf/projection';

import type {
  FirstDisplayHandoffV1,
  FirstDisplayOwnershipCapsuleV1,
} from './first_display_contracts';

const HASH = /^[0-9a-f]{64}$/;
const MAX_U32 = 4_294_967_295;
/** Seal inert ordinary data before the runtime download; semantic authority stays at takeover. */
export function snapshotFirstDisplayHandoffEnvelopeV1(
  candidate: unknown
): Readonly<Record<string, unknown>> | undefined {
  try {
    const serialized = JSON.stringify(candidate);
    if (typeof serialized !== 'string' || serialized.length > 9 * 1024 * 1024) return undefined;
    const handoff = JSON.parse(serialized) as Record<string, unknown>;
    return handoff &&
      !Array.isArray(handoff) &&
      handoff['captureVersion'] === 1 &&
      typeof handoff['releaseId'] === 'string' &&
      HASH.test(handoff['releaseId']) &&
      Number.isInteger(handoff['generation']) &&
      (handoff['generation'] as number) >= 1 &&
      (handoff['generation'] as number) <= MAX_U32 &&
      Number.isInteger(handoff['mutationRevision']) &&
      (handoff['mutationRevision'] as number) >= 0 &&
      (handoff['mutationRevision'] as number) <= MAX_U32 &&
      Number.isInteger(handoff['identityCount']) &&
      (handoff['identityCount'] as number) >= 0 &&
      (handoff['identityCount'] as number) <= 512
      ? Object.freeze(handoff)
      : undefined;
  } catch {
    return undefined;
  }
}

export type FirstDisplayHandoffOwnerState =
  'observing' | 'sealing' | 'finalized' | 'failed' | 'disposed';

export interface FirstDisplayHandoffOwnerOptions {
  readonly releaseId: string;
  readonly generation: number;
  readonly initialMutationRevision?: number;
  readonly isCurrentGeneration: () => boolean;
  readonly isTerminal: () => boolean;
  readonly isPainted: () => boolean;
  /** Closes all old-epoch work ingress synchronously before the final snapshot. */
  readonly closeIngress: () => void;
  readonly onFailure: (reason: BootFailureReason) => void;
}

export interface FinalizedFirstDisplayHandoffV1 {
  readonly handoff: Readonly<Record<string, unknown>>;
  readonly capsule: FirstDisplayOwnershipCapsuleV1<object>;
}

export interface FirstDisplayHandoffOwner {
  readonly state: FirstDisplayHandoffOwnerState;
  readonly mutationRevision: number;
  readonly observeMutation: () => boolean;
  readonly finalize: (
    capture: () => Readonly<{ candidate: unknown; identities: readonly object[] }> | undefined
  ) => FinalizedFirstDisplayHandoffV1 | undefined;
  readonly dispose: () => void;
}

export interface FirstDisplayTakeoverOptions {
  readonly finalized: FinalizedFirstDisplayHandoffV1;
  readonly outline: unknown;
  readonly boot?: unknown;
  readonly isCurrentGeneration: () => boolean;
  readonly authenticateRuntimeScript: () => boolean;
  readonly currentMutationRevision: () => number;
  readonly quiesceAgent: () => void;
  readonly detachCommittedArtifacts: () => void;
  readonly disposeAgent: () => void;
  /** Persistent-core validator; executes before either owner mutates state. */
  readonly validateHandoff: (
    handoff: unknown,
    outline: unknown,
    boot?: unknown
  ) => FirstDisplayHandoffV1 | undefined;
  readonly activatePersistent: (
    handoff: FirstDisplayHandoffV1,
    identities: readonly object[],
    own: (dispose: () => void) => void
  ) => void;
  readonly commitPersistent: () => void;
  readonly onFailure: (reason: BootFailureReason) => void;
}

export interface PreparedFirstDisplayTakeoverOptions extends Omit<
  FirstDisplayTakeoverOptions,
  'activatePersistent' | 'commitPersistent' | 'validateHandoff'
> {
  readonly prepared: PreparedKernelTakeover;
}

/** Stack-local old-owner state accepted only by the release-matched persistent finalizer. */
export type FirstDisplayAgentCaptureSourceV1 = readonly [
  handoff: Readonly<{
    releaseId: string;
    generation: number;
    integrationConfigDigest: string;
    slices: readonly FirstDisplaySliceId[];
  }>,
  batch: readonly [
    projectionDigest: string,
    outcomes: readonly (readonly [slotId: string, kind: FirstDisplayProjectedKind])[],
  ],
  slotResults: ReadonlyMap<string, string>,
  reasons: ReadonlyMap<string, string | null>,
  acceptedTrace: ReadonlyMap<string, Readonly<{ atMs: number; historySequence: number }>>,
  parserState: readonly (readonly [
    string,
    readonly (readonly [string, string | number | boolean | null])[],
  ])[],
  gptCycles: readonly FirstDisplayGptCaptureCycleV1[] | undefined,
  diagnostics: FirstDisplayGptDiagnosticsCaptureV1 | undefined,
  render: FirstDisplayRenderCaptureV1 | undefined,
  timing: readonly [
    startedAtMs: number,
    firstActionAtMs: number | null,
    terminalAtMs: number,
    paintAtMs: number,
    currentTimeMs: number,
  ],
  nextTraceSequence: number,
  mutationRevision: number,
];

export type FirstDisplayAgentCaptureFinalizerV1 = (
  source: FirstDisplayAgentCaptureSourceV1
) => FinalizedFirstDisplayHandoffV1 | undefined;

export function createFirstDisplayOwnershipCapsuleV1<T extends object>(
  releaseId: string,
  generation: number,
  identities: readonly T[]
): FirstDisplayOwnershipCapsuleV1<T> | undefined {
  if (
    !HASH.test(releaseId) ||
    !Number.isInteger(generation) ||
    generation < 1 ||
    generation > MAX_U32 ||
    identities.length > 512
  ) {
    return undefined;
  }
  const accepted = [...identities];
  if (
    accepted.some((identity) => typeof identity !== 'object' || identity === null) ||
    new Set(accepted).size !== accepted.length
  ) {
    return undefined;
  }
  let live: T[] | undefined = accepted;
  return Object.freeze({
    releaseId,
    generation,
    consume: (candidateReleaseId: string, candidateGeneration: number) => {
      if (!live || candidateReleaseId !== releaseId || candidateGeneration !== generation) {
        return undefined;
      }
      const result = Object.freeze(live);
      live = undefined;
      return result;
    },
    clear: () => {
      live = undefined;
    },
  });
}

/** Materialize the compact data envelope and one-use identities in the takeover task. */
export function finalizeFirstDisplayAgentCaptureV1(
  source: FirstDisplayAgentCaptureSourceV1
): FinalizedFirstDisplayHandoffV1 | undefined {
  try {
    const [
      handoffSource,
      batch,
      slotResults,
      reasons,
      acceptedTrace,
      parserState,
      gptCycles,
      diagnosticsSource,
      renderSource,
      timing,
      nextTraceSequence,
      mutationRevision,
    ] = source;
    const cycles = gptCycles ?? Object.freeze([]);
    const diagnostics =
      diagnosticsSource ?? Object.freeze([Object.freeze([]), Object.freeze([]), 1, 0, 0] as const);
    const render =
      renderSource ??
      Object.freeze([Object.freeze([]), Object.freeze([]), timing[4], 1, 1] as const);
    if (
      diagnostics[0].length !== cycles.length ||
      diagnostics[0].some((cycle) => {
        const physical = cycles.find((candidate) => candidate[0] === cycle[0]);
        return !physical || physical[4] !== cycle[1];
      })
    ) {
      return undefined;
    }
    const identities = [
      ...cycles.map((cycle) => cycle[5]),
      ...render[0].map((artifact) => artifact[2]),
    ];
    const capsule = createFirstDisplayOwnershipCapsuleV1(
      handoffSource.releaseId,
      handoffSource.generation,
      identities
    );
    if (!capsule) return undefined;
    const results = batch[1].map(([slotId]) => {
      const accepted = acceptedTrace.get(slotId);
      return [
        slotResults.get(slotId) ?? 'failed',
        reasons.get(slotId) ?? null,
        accepted?.atMs ?? null,
        accepted?.historySequence ?? null,
      ];
    });
    const handoff = Object.freeze({
      captureVersion: 1,
      releaseId: handoffSource.releaseId,
      generation: handoffSource.generation,
      data: [
        batch[0],
        handoffSource.integrationConfigDigest,
        handoffSource.slices,
        results,
        cycles.map((cycle) => cycle.slice(0, 5)),
        render[1],
        render[0].map((artifact) => [
          artifact[0],
          artifact[1],
          artifact[5],
          artifact[3],
          artifact[4],
          artifact[6],
        ]),
        parserState,
        [diagnostics[1], diagnostics[3], diagnostics[4]],
        timing.slice(0, 4),
        [results.length + 1, render[2], render[3], render[4]],
        diagnostics[0].map((cycle) => ({
          slotId: cycle[0],
          token: cycle[1],
          nextCycleOrdinal: cycle[2],
          unknownPriorCycle: cycle[3],
          quarantines: cycle[4],
          records: cycle[5].map((record) => ({
            ordinal: record[0],
            responseIdentifier: record[1],
            seen: record[2],
            state: record[3],
          })),
        })),
        nextTraceSequence,
        diagnostics[2],
      ],
      mutationRevision,
      identityCount: identities.length,
    });
    return Object.freeze({ handoff, capsule });
  } catch {
    return undefined;
  }
}

/**
 * Own the final old-epoch revision and one-use object capsule. `finalize` is
 * deliberately synchronous: after ingress closes, no browser task can interleave
 * before the immutable snapshot and capsule are bound to the same revision.
 */
export function createFirstDisplayHandoffOwner(
  options: FirstDisplayHandoffOwnerOptions
): FirstDisplayHandoffOwner {
  let state: FirstDisplayHandoffOwnerState = 'observing';
  let revision = options.initialMutationRevision ?? 0;
  let liveCapsule: FirstDisplayOwnershipCapsuleV1<object> | undefined;
  let failurePublished = false;

  const publishFailure = (): undefined => {
    liveCapsule?.clear();
    liveCapsule = undefined;
    state = 'failed';
    if (!failurePublished) {
      failurePublished = true;
      try {
        options.onFailure('bundle_partial');
      } catch {
        // Failure reporting cannot preserve transferable object authority.
      }
    }
    return undefined;
  };

  if (
    !HASH.test(options.releaseId) ||
    !Number.isInteger(options.generation) ||
    options.generation < 1 ||
    options.generation > MAX_U32 ||
    !Number.isInteger(revision) ||
    revision < 0 ||
    revision > MAX_U32
  ) {
    publishFailure();
  }

  return Object.freeze({
    get state() {
      return state;
    },
    get mutationRevision() {
      return revision;
    },
    observeMutation: (): boolean => {
      if (state !== 'observing' && state !== 'sealing') return false;
      if (revision >= MAX_U32) {
        publishFailure();
        return false;
      }
      revision += 1;
      return true;
    },
    finalize: (
      capture: () => Readonly<{ candidate: unknown; identities: readonly object[] }> | undefined
    ): FinalizedFirstDisplayHandoffV1 | undefined => {
      if (state !== 'observing') return publishFailure();
      try {
        if (
          typeof capture !== 'function' ||
          !options.isCurrentGeneration() ||
          !options.isTerminal() ||
          !options.isPainted()
        ) {
          return publishFailure();
        }
        state = 'sealing';
        options.closeIngress();
        const captured = capture();
        if (!captured) return publishFailure();
        const handoff = snapshotFirstDisplayHandoffEnvelopeV1(captured.candidate);
        if (
          !handoff ||
          handoff['releaseId'] !== options.releaseId ||
          handoff['generation'] !== options.generation ||
          handoff['mutationRevision'] !== revision ||
          captured.identities.length !== handoff['identityCount'] ||
          new Set(captured.identities).size !== captured.identities.length
        ) {
          return publishFailure();
        }
        const capsule = createFirstDisplayOwnershipCapsuleV1(
          options.releaseId,
          options.generation,
          captured.identities
        );
        if (!capsule) return publishFailure();
        liveCapsule = capsule;
        state = 'finalized';
        return Object.freeze({ handoff, capsule });
      } catch {
        return publishFailure();
      }
    },
    dispose: (): void => {
      if (state === 'disposed') return;
      liveCapsule?.clear();
      liveCapsule = undefined;
      state = 'disposed';
    },
  });
}

/** Execute the non-yielding old-owner to persistent-owner transfer in one call stack. */
export function performFirstDisplayTakeoverV1(options: FirstDisplayTakeoverOptions): boolean {
  const persistentDisposers: Array<() => void> = [];
  let ownershipOpen = true;
  let failurePublished = false;
  const fail = (): false => {
    ownershipOpen = false;
    for (let index = persistentDisposers.length - 1; index >= 0; index -= 1) {
      try {
        persistentDisposers[index]?.();
      } catch {
        // Continue unwinding every independently activated persistent effect.
      }
    }
    persistentDisposers.length = 0;
    options.finalized.capsule.clear();
    if (!failurePublished) {
      failurePublished = true;
      try {
        options.onFailure('bundle_partial');
      } catch {
        // Failure publication cannot restore either ownership epoch.
      }
    }
    return false;
  };

  try {
    const handoff = options.validateHandoff(
      options.finalized.handoff,
      options.outline,
      options.boot
    );
    const { capsule } = options.finalized;
    if (
      !handoff ||
      !options.isCurrentGeneration() ||
      !options.authenticateRuntimeScript() ||
      options.currentMutationRevision() !== handoff.mutationRevision
    ) {
      return fail();
    }

    options.quiesceAgent();
    if (
      !options.isCurrentGeneration() ||
      options.currentMutationRevision() !== handoff.mutationRevision
    ) {
      return fail();
    }
    const identities = capsule.consume(handoff.releaseId, handoff.generation);
    if (!identities) return fail();

    options.detachCommittedArtifacts();
    options.disposeAgent();
    options.activatePersistent(handoff, identities, (dispose) => {
      if (!ownershipOpen || typeof dispose !== 'function') {
        throw new TypeError('tsjs');
      }
      persistentDisposers.push(dispose);
    });
    ownershipOpen = false;
    if (
      !options.isCurrentGeneration() ||
      !options.authenticateRuntimeScript() ||
      options.currentMutationRevision() !== handoff.mutationRevision
    ) {
      return fail();
    }
    options.commitPersistent();
    persistentDisposers.length = 0;
    return true;
  } catch {
    return fail();
  }
}

/** Bind the validated old-epoch snapshot to one prepared persistent activation transaction. */
export function coordinatePreparedFirstDisplayTakeoverV1(
  options: PreparedFirstDisplayTakeoverOptions
): boolean {
  return performFirstDisplayTakeoverV1({
    finalized: options.finalized,
    outline: options.outline,
    ...(options.boot === undefined ? {} : { boot: options.boot }),
    validateHandoff: options.prepared.validateHandoff,
    isCurrentGeneration: options.isCurrentGeneration,
    authenticateRuntimeScript: options.authenticateRuntimeScript,
    currentMutationRevision: options.currentMutationRevision,
    quiesceAgent: options.quiesceAgent,
    detachCommittedArtifacts: options.detachCommittedArtifacts,
    disposeAgent: options.disposeAgent,
    activatePersistent: (handoff, identities, own) => {
      own(options.prepared.rollback);
      options.prepared.activate(
        Object.freeze({
          version: 1 as const,
          adoptInitialDisplay: true as const,
          handoff,
          identities,
        })
      );
    },
    commitPersistent: options.prepared.commit,
    onFailure: options.onFailure,
  });
}
