import type {
  FirstDisplayGoogletagBatchInput,
  FirstDisplayGptBoundCycleV1,
  FirstDisplayGptRenderResult,
} from './adapters/googletag';
import type { FirstDisplayDriver, FirstDisplayTerminalResult } from './agent';
import type { FirstDisplayGptProtocolV1 } from './leaf/gpt_protocol';
import type { FirstDisplayBatchOutcomeV1, FirstDisplayBatchV1 } from './leaf/projection';

const ACTION_KINDS = new Set(['gpt_adm', 'aps']);
const TERMINAL_RESULTS = new Set(['accepted', 'failed', 'cancelled']);

export interface FirstDisplayRenderBridgeV1 {
  readonly bind: (
    cycle: FirstDisplayGptBoundCycleV1,
    onTerminal: (result: FirstDisplayTerminalResult) => void
  ) => boolean;
  readonly recordGam: (
    cycle: FirstDisplayGptBoundCycleV1,
    result: FirstDisplayGptRenderResult
  ) => boolean;
  readonly recordFailure: (cycle: FirstDisplayGptBoundCycleV1) => boolean;
  readonly sealTsAdmission: () => void;
  readonly closeIngress: () => boolean;
  readonly captureHandoff: () => FirstDisplayRenderHandoffV1 | undefined;
  readonly detachCommittedArtifacts: () => boolean;
  readonly dispose: () => void;
}

export interface FirstDisplayRenderHandoffArtifactV1 {
  readonly identity: object;
  readonly kind: 'gpt_adm' | 'aps';
  readonly owner: 'trusted_server' | 'publisher';
  readonly slotId: string;
  readonly token: string;
}

export interface FirstDisplayRenderHandoffV1 {
  readonly artifacts: readonly FirstDisplayRenderHandoffArtifactV1[];
  readonly nextTicketOrdinal: number;
  readonly tombstones: readonly Readonly<{
    kind: 'ticket';
    value: string;
    expiresAtMs: number;
    ordinal: number;
  }>[];
}

export interface FirstDisplayProjectedDriverOptionsV1 {
  readonly batch: FirstDisplayBatchV1;
  readonly gpt?: FirstDisplayGptProtocolV1;
  readonly gptInput: Omit<FirstDisplayGoogletagBatchInput, 'projection'>;
  readonly renderer: FirstDisplayRenderBridgeV1;
}

function sameCycle(
  expected: FirstDisplayGptBoundCycleV1,
  candidate: FirstDisplayGptBoundCycleV1
): boolean {
  return (
    candidate.slotId === expected.slotId &&
    candidate.bid === expected.bid &&
    candidate.element === expected.element &&
    candidate.placement === expected.placement &&
    candidate.physicalSlot === expected.physicalSlot &&
    candidate.ownership === expected.ownership
  );
}

/** Join GPT's exact physical cycle to the independent APS/ADM render authority. */
export function createFirstDisplayProjectedDriver(
  options: FirstDisplayProjectedDriverOptionsV1
): FirstDisplayDriver {
  const expected = options.batch.outcomes.filter(({ kind }) => ACTION_KINDS.has(kind));
  const expectedBySlot = new Map(expected.map((outcome) => [outcome.slotId, outcome]));
  const bound = new Map<string, FirstDisplayGptBoundCycleV1>();
  const settled = new Set<string>();
  const settledResults = new Map<string, FirstDisplayTerminalResult>();
  const gptBatch =
    expected.length === 0
      ? undefined
      : options.gpt?.createBatch({
          ...options.gptInput,
          projection: options.batch.projection,
        });
  if (expected.length > 0 && !gptBatch) {
    throw new TypeError('first-display GPT protocol is unavailable');
  }
  let started = false;
  let disposed = false;
  let sealed = false;
  let actionStarted = false;
  let ingressClosed = false;
  let handoffCaptured = false;
  let committedArtifactsDetached = false;
  let onTerminal: ((slotId: string, result: FirstDisplayTerminalResult) => void) | undefined;

  const settle = (slotId: string, result: FirstDisplayTerminalResult): boolean => {
    if (
      disposed ||
      !expectedBySlot.has(slotId) ||
      settled.has(slotId) ||
      !TERMINAL_RESULTS.has(result)
    ) {
      return false;
    }
    settled.add(slotId);
    settledResults.set(slotId, result);
    onTerminal?.(slotId, result);
    return true;
  };

  return Object.freeze({
    start: (
      outcomes: readonly FirstDisplayBatchOutcomeV1[],
      onFirstAction: () => boolean,
      terminal: (slotId: string, result: FirstDisplayTerminalResult) => void
    ): void => {
      if (started || disposed) throw new TypeError('first-display driver is not startable');
      if (
        outcomes.length !== expected.length ||
        outcomes.some((outcome, index) => {
          const row = expected[index];
          return !row || row.slotId !== outcome.slotId || row.kind !== outcome.kind;
        })
      ) {
        throw new TypeError('invalid first-display action list');
      }
      started = true;
      onTerminal = terminal;
      const accepted = gptBatch?.start({
        onBound: (cycle): void => {
          const action = expectedBySlot.get(cycle.slotId);
          const bidIndex = options.batch.projection.bids.indexOf(cycle.bid);
          const expectedPlacement = options.batch.projection.slots.find(
            ({ slot }) => slot === cycle.slotId
          );
          if (
            !action ||
            bound.has(cycle.slotId) ||
            bidIndex < 0 ||
            options.batch.projection.bids[bidIndex]?.slot !== cycle.slotId ||
            cycle.placement !== expectedPlacement
          ) {
            settle(cycle.slotId, 'failed');
            return;
          }
          bound.set(cycle.slotId, cycle);
          if (!options.renderer.bind(cycle, (result) => settle(cycle.slotId, result))) {
            settle(cycle.slotId, 'failed');
          }
        },
        onFailure: (slotId): void => {
          const cycle = bound.get(slotId);
          if (cycle) options.renderer.recordFailure(cycle);
          settle(slotId, 'failed');
        },
        onFirstAction: (): boolean => {
          if (actionStarted || disposed) return false;
          actionStarted = true;
          return onFirstAction();
        },
        onRenderEnded: (cycle, result): void => {
          const exact = bound.get(cycle.slotId);
          if (!exact || !sameCycle(exact, cycle)) {
            settle(cycle.slotId, 'failed');
            return;
          }
          if (!options.renderer.recordGam(exact, result)) settle(cycle.slotId, 'failed');
        },
      });
      if (accepted !== true) throw new TypeError('first-display GPT batch was rejected');
    },
    sealTsAdmission: (): void => {
      if (sealed || disposed || settled.size !== expected.length) {
        throw new TypeError('first-display driver cannot seal');
      }
      sealed = true;
      options.renderer.sealTsAdmission();
    },
    closeIngress: (): boolean => {
      if (disposed || !sealed || ingressClosed) return false;
      if (gptBatch && !gptBatch.closeIngress()) return false;
      if (!options.renderer.closeIngress()) return false;
      ingressClosed = true;
      return true;
    },
    captureHandoff: () => {
      if (disposed || !ingressClosed || handoffCaptured) return undefined;
      const gptCycles = gptBatch?.captureHandoff() ?? Object.freeze([]);
      const render = options.renderer.captureHandoff();
      if (!render) return undefined;
      const acceptedIds = new Set(
        [...settledResults.entries()]
          .filter(([, result]) => result === 'accepted')
          .map(([slotId]) => slotId)
      );
      const cycles = gptCycles.filter(({ slotId }) => acceptedIds.has(slotId));
      if (
        cycles.length !== acceptedIds.size ||
        render.artifacts.some(({ slotId }) => !acceptedIds.has(slotId))
      ) {
        return undefined;
      }
      const identities = [
        ...cycles.map(({ physicalSlot }) => physicalSlot),
        ...render.artifacts.map(({ identity }) => identity),
      ];
      if (new Set(identities).size !== identities.length) return undefined;
      handoffCaptured = true;
      return Object.freeze({
        artifacts: render.artifacts,
        cycles: Object.freeze(cycles),
        identities: Object.freeze(identities),
        nextTicketOrdinal: render.nextTicketOrdinal,
        tombstones: render.tombstones,
      });
    },
    detachCommittedArtifacts: (): boolean => {
      if (disposed || !ingressClosed || !handoffCaptured || committedArtifactsDetached) {
        return false;
      }
      const acceptedIds = [...settledResults.entries()]
        .filter(([, result]) => result === 'accepted')
        .map(([slotId]) => slotId);
      if (gptBatch && !gptBatch.detachCommittedSlots(acceptedIds)) return false;
      if (!options.renderer.detachCommittedArtifacts()) return false;
      committedArtifactsDetached = true;
      return true;
    },
    dispose: (): void => {
      if (disposed) return;
      disposed = true;
      gptBatch?.dispose();
      options.renderer.dispose();
      bound.clear();
      settledResults.clear();
      onTerminal = undefined;
    },
  });
}
