import type { BootFailureReason } from '../kernel/fallback';

import {
  createFirstDisplayOwnershipCapsuleV1,
  snapshotFirstDisplayHandoffV1,
  snapshotTakeoverOutlineV1,
  type FirstDisplayHandoffV1,
  type FirstDisplayOwnershipCapsuleV1,
} from './contracts';

const HASH = /^[0-9a-f]{64}$/;
const MAX_U32 = 4_294_967_295;

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
    candidate: unknown,
    identities: readonly object[]
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
  readonly activatePersistent: (
    handoff: FirstDisplayHandoffV1,
    identities: readonly object[],
    own: (dispose: () => void) => void
  ) => void;
  readonly commitPersistent: () => void;
  readonly onFailure: (reason: BootFailureReason) => void;
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
      if (state !== 'observing') return false;
      if (revision >= MAX_U32) {
        publishFailure();
        return false;
      }
      revision += 1;
      return true;
    },
    finalize: (
      candidate: unknown,
      identities: readonly object[]
    ): FinalizedFirstDisplayHandoffV1 | undefined => {
      if (state !== 'observing') return publishFailure();
      try {
        if (!options.isCurrentGeneration() || !options.isTerminal() || !options.isPainted()) {
          return publishFailure();
        }
        state = 'sealing';
        options.closeIngress();
        const handoff = snapshotFirstDisplayHandoffV1(candidate);
        if (
          !handoff ||
          handoff.releaseId !== options.releaseId ||
          handoff.generation !== options.generation ||
          handoff.mutationRevision !== revision
        ) {
          return publishFailure();
        }
        const capsule = createFirstDisplayOwnershipCapsuleV1(
          options.releaseId,
          options.generation,
          identities
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
    const outline = snapshotTakeoverOutlineV1(options.outline);
    const { handoff, capsule } = options.finalized;
    if (
      !outline ||
      outline.releaseId !== handoff.releaseId ||
      outline.generation !== handoff.generation ||
      outline.projectionDigest !== handoff.projectionDigest ||
      outline.slotCount !== handoff.slots.length ||
      outline.outcomeCount !== handoff.slots.length ||
      outline.slices.length !== handoff.slices.length ||
      outline.slices.some((id, index) => id !== handoff.slices[index]) ||
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
        throw new TypeError('persistent takeover disposer registration is closed');
      }
      persistentDisposers.push(dispose);
    });
    ownershipOpen = false;
    if (
      !options.isCurrentGeneration() ||
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
