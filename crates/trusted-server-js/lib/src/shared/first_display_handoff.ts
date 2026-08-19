import type { BootFailureReason } from '../kernel/fallback_surface';
import type { PreparedKernelTakeover } from '../kernel/integration_registry';

import type {
  FirstDisplayHandoffV1,
  FirstDisplayOwnershipCapsuleV1,
} from './first_display_contracts';

const HASH = /^[0-9a-f]{64}$/;
const MAX_U32 = 4_294_967_295;
const HANDOFF_FIELDS = Object.freeze([
  'version',
  'releaseId',
  'generation',
  'projectionDigest',
  'integrationConfigDigest',
  'slices',
  'slots',
  'attempts',
  'tombstones',
  'artifacts',
  'parserState',
  'gptDiagnostics',
  'timing',
  'highWater',
  'cycles',
  'trace',
  'mutationRevision',
]);

/** Seal inert ordinary data before the runtime download; semantic authority stays at takeover. */
function snapshotFirstDisplayHandoffEnvelopeV1(
  candidate: unknown
): FirstDisplayHandoffV1 | undefined {
  try {
    const serialized = JSON.stringify(candidate);
    if (typeof serialized !== 'string' || serialized.length > 9 * 1024 * 1024) return undefined;
    const snapshot = JSON.parse(serialized) as unknown;
    if (typeof snapshot !== 'object' || snapshot === null || Array.isArray(snapshot)) {
      return undefined;
    }
    const handoff = snapshot as unknown as FirstDisplayHandoffV1;
    const keys = Reflect.ownKeys(snapshot);
    if (
      keys.length !== HANDOFF_FIELDS.length ||
      !keys.every((key) => typeof key === 'string' && HANDOFF_FIELDS.includes(key)) ||
      handoff.version !== 1 ||
      !HASH.test(handoff.releaseId) ||
      !Number.isInteger(handoff.generation) ||
      handoff.generation < 1 ||
      handoff.generation > MAX_U32 ||
      !HASH.test(handoff.projectionDigest) ||
      !HASH.test(handoff.integrationConfigDigest) ||
      !Number.isInteger(handoff.mutationRevision) ||
      handoff.mutationRevision < 0 ||
      handoff.mutationRevision > MAX_U32 ||
      !Array.isArray(handoff.slices) ||
      !Array.isArray(handoff.slots) ||
      !Array.isArray(handoff.attempts) ||
      !Array.isArray(handoff.tombstones) ||
      !Array.isArray(handoff.artifacts) ||
      !Array.isArray(handoff.parserState) ||
      !Array.isArray(handoff.cycles) ||
      handoff.attempts.some(
        (attempt) => !['accepted', 'no_bid', 'failed', 'cancelled'].includes(String(attempt.state))
      )
    ) {
      return undefined;
    }
    return Object.freeze(handoff);
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
  readonly handoff: FirstDisplayHandoffV1;
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
  readonly isCurrentGeneration: () => boolean;
  readonly authenticateRuntimeScript: () => boolean;
  readonly currentMutationRevision: () => number;
  readonly quiesceAgent: () => void;
  readonly detachCommittedArtifacts: () => void;
  readonly disposeAgent: () => void;
  /** Persistent-core validator; executes before either owner mutates state. */
  readonly validateHandoff: (
    handoff: unknown,
    outline: unknown
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

function createFirstDisplayOwnershipCapsuleV1<T extends object>(
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
          handoff.releaseId !== options.releaseId ||
          handoff.generation !== options.generation ||
          handoff.mutationRevision !== revision ||
          captured.identities.length !== handoff.cycles.length + handoff.artifacts.length ||
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
    const handoff = options.validateHandoff(options.finalized.handoff, options.outline);
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
