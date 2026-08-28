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
    onTerminal: (result: FirstDisplayTerminalResult, reason: string | null) => void
  ) => boolean;
  readonly recordGam: (
    cycle: FirstDisplayGptBoundCycleV1,
    result: FirstDisplayGptRenderResult
  ) => boolean;
  readonly recordFailure: (cycle: FirstDisplayGptBoundCycleV1) => boolean;
  readonly retire: (cycle: FirstDisplayGptBoundCycleV1) => boolean;
  readonly sweepCommittedArtifacts: () => number;
  readonly sealTsAdmission: () => void;
  readonly closeIngress: () => boolean;
  readonly captureHandoff: () => FirstDisplayRenderCaptureV1 | undefined;
  readonly detachCommittedArtifacts: () => boolean;
  readonly dispose: () => void;
}

/** Compact authenticated capability passed from the render slice to the inline owner. */
export type FirstDisplayRenderBridgeCapabilityV1 = readonly [
  bind: FirstDisplayRenderBridgeV1['bind'],
  recordGam: FirstDisplayRenderBridgeV1['recordGam'],
  recordFailure: FirstDisplayRenderBridgeV1['recordFailure'],
  retire: FirstDisplayRenderBridgeV1['retire'],
  sweepCommittedArtifacts: FirstDisplayRenderBridgeV1['sweepCommittedArtifacts'],
  sealTsAdmission: FirstDisplayRenderBridgeV1['sealTsAdmission'],
  closeIngress: FirstDisplayRenderBridgeV1['closeIngress'],
  captureHandoff: () => FirstDisplayRenderCaptureV1 | undefined,
  detachCommittedArtifacts: FirstDisplayRenderBridgeV1['detachCommittedArtifacts'],
  dispose: FirstDisplayRenderBridgeV1['dispose'],
];

/** Compact cross-artifact render state; live identity remains tuple-local until takeover. */
export type FirstDisplayRenderCaptureV1 = readonly [
  artifacts: readonly (readonly [
    hostPosition: string | null,
    hostPositionPriority: string | null,
    identity: object,
    kind: 'gpt_adm' | 'aps',
    owner: 'trusted_server' | 'publisher',
    slotId: string,
    token: string,
  ])[],
  tombstones: readonly (readonly [
    kind: 'reservation' | 'ticket',
    value: string,
    expiresAtMs: number,
    ordinal: number,
  ])[],
  clockEpochMs: number,
  nextReservationOrdinal: number,
  nextTicketOrdinal: number,
];

export interface FirstDisplayRenderHandoffArtifactV1 {
  readonly hostPosition: string | null;
  readonly hostPositionPriority: string | null;
  readonly identity: object;
  readonly kind: 'gpt_adm' | 'aps';
  readonly owner: 'trusted_server' | 'publisher';
  readonly slotId: string;
  readonly token: string;
}

export interface FirstDisplayRenderHandoffV1 {
  readonly artifacts: readonly FirstDisplayRenderHandoffArtifactV1[];
  readonly clockEpochMs: number;
  readonly nextReservationOrdinal: number;
  readonly nextTicketOrdinal: number;
  readonly tombstones: readonly Readonly<{
    kind: 'reservation' | 'ticket';
    value: string;
    expiresAtMs: number;
    ordinal: number;
  }>[];
}

export interface FirstDisplayProjectedDriverOptionsV1 {
  readonly batch: FirstDisplayBatchV1;
  readonly gpt?: FirstDisplayGptProtocolV1;
  readonly gptInput: readonly [
    browser: FirstDisplayGoogletagBatchInput[0],
    clearTimer: FirstDisplayGoogletagBatchInput[1],
    document: FirstDisplayGoogletagBatchInput[2],
    setTimer: FirstDisplayGoogletagBatchInput[3],
    diagnosticsActive?: FirstDisplayGoogletagBatchInput[5],
    onNativeMutation?: FirstDisplayGoogletagBatchInput[6],
  ];
  readonly renderer: FirstDisplayRenderBridgeV1;
}

function sameCycle(
  expected: FirstDisplayGptBoundCycleV1,
  candidate: FirstDisplayGptBoundCycleV1
): boolean {
  return (
    candidate[6] === expected[6] &&
    candidate[0] === expected[0] &&
    candidate[1] === expected[1] &&
    candidate[2] === expected[2] &&
    candidate[5] === expected[5] &&
    candidate[4] === expected[4] &&
    candidate[3] === expected[3] &&
    candidate[7] === expected[7]
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
      : options.gpt?.[2](
          Object.freeze([
            options.gptInput[0],
            options.gptInput[1],
            options.gptInput[2],
            options.gptInput[3],
            options.batch.projection,
            options.gptInput[4],
            options.gptInput[5],
          ])
        );
  if (expected.length > 0 && !gptBatch) {
    throw new TypeError('tsjs');
  }
  let started = false;
  let disposed = false;
  let sealed = false;
  let actionStarted = false;
  let ingressClosed = false;
  let handoffCaptured = false;
  let committedArtifactsDetached = false;
  let onTerminal:
    | ((slotId: string, result: FirstDisplayTerminalResult, reason: string | null) => void)
    | undefined;

  const settle = (
    slotId: string,
    result: FirstDisplayTerminalResult,
    reason: string | null
  ): boolean => {
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
    onTerminal?.(slotId, result, reason);
    return true;
  };

  return Object.freeze({
    start: (
      outcomes: readonly FirstDisplayBatchOutcomeV1[],
      onFirstAction: () => boolean,
      terminal: (slotId: string, result: FirstDisplayTerminalResult, reason: string | null) => void
    ): void => {
      if (started || disposed) throw new TypeError('tsjs');
      if (
        outcomes.length !== expected.length ||
        outcomes.some((outcome, index) => {
          const row = expected[index];
          return !row || row.slotId !== outcome.slotId || row.kind !== outcome.kind;
        })
      ) {
        throw new TypeError('tsjs');
      }
      started = true;
      onTerminal = terminal;
      const accepted = gptBatch?.[0]([
        (cycle): void => {
          const action = expectedBySlot.get(cycle[6]);
          const bidIndex = options.batch.projection.bids.indexOf(cycle[0]);
          const expectedPlacement = options.batch.projection.slots.find(
            ({ slot }) => slot === cycle[6]
          );
          if (
            !action ||
            bound.has(cycle[6]) ||
            bidIndex < 0 ||
            options.batch.projection.bids[bidIndex]?.slot !== cycle[6] ||
            cycle[5] !== expectedPlacement
          ) {
            settle(cycle[6], 'failed', 'gpt_request_failed');
            return;
          }
          bound.set(cycle[6], cycle);
          if (!options.renderer.bind(cycle, (result, reason) => settle(cycle[6], result, reason))) {
            settle(cycle[6], 'failed', 'internal_error');
          }
        },
        (slotId, reason): void => {
          const cycle = bound.get(slotId);
          if (cycle) options.renderer.recordFailure(cycle);
          settle(slotId, 'failed', reason);
        },
        (): boolean => {
          if (actionStarted || disposed) return false;
          actionStarted = true;
          return onFirstAction();
        },
        (cycle, result): void => {
          const exact = bound.get(cycle[6]);
          if (!exact || !sameCycle(exact, cycle)) {
            settle(cycle[6], 'failed', 'gpt_request_failed');
            return;
          }
          if (!options.renderer.recordGam(exact, result)) {
            settle(cycle[6], 'failed', 'gpt_request_failed');
          }
        },
        (cycle): void => {
          const exact = bound.get(cycle[6]);
          if (exact && sameCycle(exact, cycle)) options.renderer.retire(exact);
        },
      ]);
      if (accepted !== true) throw new TypeError('tsjs');
    },
    sealTsAdmission: (): void => {
      if (sealed || disposed || settled.size !== expected.length) {
        throw new TypeError('tsjs');
      }
      sealed = true;
      options.renderer.sealTsAdmission();
    },
    sweepCommittedArtifacts: (): number =>
      disposed ? 0 : options.renderer.sweepCommittedArtifacts(),
    closeIngress: (): boolean => {
      if (disposed || !sealed || ingressClosed) return false;
      const acceptedIds = [...settledResults.entries()]
        .filter(([, result]) => result === 'accepted')
        .map(([slotId]) => slotId);
      if (gptBatch && !gptBatch[1](acceptedIds)) return false;
      if (!options.renderer.closeIngress()) return false;
      ingressClosed = true;
      return true;
    },
    captureHandoff: () => {
      if (disposed || !ingressClosed || handoffCaptured) return undefined;
      const gptCycles = gptBatch?.[2]() ?? Object.freeze([]);
      const gptDiagnostics =
        gptBatch?.[3]() ?? Object.freeze([Object.freeze([]), Object.freeze([]), 1, 0, 0] as const);
      const render = options.renderer.captureHandoff();
      if (!render) return undefined;
      const acceptedIds = new Set(
        [...settledResults.entries()]
          .filter(([, result]) => result === 'accepted')
          .map(([slotId]) => slotId)
      );
      const cycles = gptCycles.flatMap((captured) => {
        const cycle = bound.get(captured[0]);
        if (
          !cycle ||
          !acceptedIds.has(captured[0]) ||
          cycle[7] !== captured[4] ||
          cycle[4] !== captured[5]
        ) {
          return [];
        }
        const element =
          cycle[1].id === captured[1] ? cycle[1] : options.gptInput[2].getElementById(captured[1]);
        const ElementConstructor = options.gptInput[2].defaultView?.HTMLElement;
        if (!ElementConstructor || !(element instanceof ElementConstructor)) return [];
        return [
          Object.freeze([
            cycle[0],
            element,
            cycle[2],
            captured[2],
            cycle[4],
            cycle[5],
            cycle[6],
            cycle[7],
            captured[3],
          ] as const),
        ];
      });
      const diagnosticCycles = gptDiagnostics[0].filter((cycle) => acceptedIds.has(cycle[0]));
      if (
        cycles.length !== acceptedIds.size ||
        diagnosticCycles.length !== cycles.length ||
        diagnosticCycles.some((cycle) => {
          const physical = cycles.find((candidate) => candidate[6] === cycle[0]);
          return !physical || physical[7] !== cycle[1];
        }) ||
        render[0].some((artifact) => !acceptedIds.has(artifact[5]))
      ) {
        return undefined;
      }
      const identities = [
        ...cycles.map((cycle) => cycle[4]),
        ...render[0].map((artifact) => artifact[2]),
      ];
      if (new Set(identities).size !== identities.length) return undefined;
      handoffCaptured = true;
      return Object.freeze({
        artifacts: Object.freeze(
          render[0].map((artifact) => ({
            hostPosition: artifact[0],
            hostPositionPriority: artifact[1],
            identity: artifact[2],
            kind: artifact[3],
            owner: artifact[4],
            slotId: artifact[5],
            token: artifact[6],
          }))
        ),
        clockEpochMs: render[2],
        cycles: Object.freeze(cycles),
        diagnosticCycles: Object.freeze(
          diagnosticCycles.map((cycle) => ({
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
          }))
        ),
        gptDiagnostics: Object.freeze({
          facts: Object.freeze([...gptDiagnostics[1]]),
          overflowCount: gptDiagnostics[3],
          dropCount: gptDiagnostics[4],
        }),
        identities: Object.freeze(identities),
        nextTraceTokenOrdinal: gptDiagnostics[2],
        nextReservationOrdinal: render[3],
        nextTicketOrdinal: render[4],
        tombstones: Object.freeze(
          render[1].map((entry) => ({
            kind: entry[0],
            value: entry[1],
            expiresAtMs: entry[2],
            ordinal: entry[3],
          }))
        ),
      });
    },
    detachCommittedArtifacts: (): boolean => {
      if (disposed || !ingressClosed || !handoffCaptured || committedArtifactsDetached) {
        return false;
      }
      const acceptedIds = [...settledResults.entries()]
        .filter(([, result]) => result === 'accepted')
        .map(([slotId]) => slotId);
      if (gptBatch && !gptBatch[4](acceptedIds)) return false;
      if (!options.renderer.detachCommittedArtifacts()) return false;
      committedArtifactsDetached = true;
      return true;
    },
    dispose: (): void => {
      if (disposed) return;
      disposed = true;
      gptBatch?.[5]();
      options.renderer.dispose();
      bound.clear();
      settledResults.clear();
      onTerminal = undefined;
    },
  });
}
