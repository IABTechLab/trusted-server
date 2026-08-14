import type {
  GptSlotTokenV1,
  GptTraceCycleOrdinalV1,
  GoogletagAdapter,
  GoogletagDiagnosticsFact,
  GoogletagDiagnosticsObserver,
  GoogletagDiagnosticsSlotSnapshot,
} from '../../adapters/googletag';
import type { FirstDisplayGptDiagnosticsV1, FirstDisplayGptFactV1 } from '../../shared/takeover';

const MAX_BUFFERED_FACTS = 512;
const DIAGNOSTICS_ONLY_EVENTS = Object.freeze([
  'slotResponseReceived',
  'slotOnload',
  'impressionViewable',
  'slotVisibilityChanged',
] as const);
const ALL_DIAGNOSTIC_EVENTS = new Set<string>([
  'slotRequested',
  'slotResponseReceived',
  'slotRenderEnded',
  'slotOnload',
  'impressionViewable',
  'slotVisibilityChanged',
]);
export interface GptDiagnosticsFactBufferOptions {
  readonly onConsumerError?: (error: unknown) => void;
  readonly onOverflow?: (droppedFacts: number) => void;
}

export interface GptDiagnosticsFactBuffer {
  readonly adoptFirstDisplay: (
    diagnostics: Readonly<FirstDisplayGptDiagnosticsV1>,
    resolveSlot?: (traceToken: string) => Readonly<GoogletagDiagnosticsSlotSnapshot> | undefined
  ) => boolean;
  readonly publish: (fact: Readonly<GoogletagDiagnosticsFact>) => boolean;
  readonly activate: (consumer: GoogletagDiagnosticsObserver) => (() => void) | undefined;
  readonly dispose: () => void;
}

/** Project the direct opaque GPT fact into the bounded data-only core trace shape. */
export function projectGptTraceFact(
  fact: Readonly<GoogletagDiagnosticsFact>
): Readonly<Record<string, unknown>> | undefined {
  try {
    const traceToken = fact.slot.traceToken;
    const cycleOrdinal = fact.slot.cycleOrdinal;
    if (
      typeof traceToken !== 'string' ||
      !/^gt1_[1-9a-z][0-9a-z]{0,6}$/.test(traceToken) ||
      traceToken.length > 11 ||
      Number.parseInt(traceToken.slice(4), 36) > 4_294_967_295 ||
      !Number.isInteger(cycleOrdinal) ||
      (cycleOrdinal ?? 0) < 1 ||
      (cycleOrdinal ?? 0) > 4_294_967_295
    ) {
      return undefined;
    }
    const slot = Object.freeze({
      token: traceToken,
      cycleOrdinal,
      ...(typeof fact.slot.elementId === 'string' && fact.slot.elementId !== ''
        ? { elementId: fact.slot.elementId }
        : {}),
    });
    return Object.freeze({
      kind: fact.kind,
      observedAtMs: fact.observedAtMs,
      slot,
      ...(typeof fact.isEmpty === 'boolean' ? { isEmpty: fact.isEmpty } : {}),
      ...(typeof fact.inViewPercentage === 'number' && Number.isFinite(fact.inViewPercentage)
        ? { inViewPercentage: fact.inViewPercentage }
        : {}),
      ...(typeof fact.responseIdentifier === 'string' && fact.responseIdentifier !== ''
        ? { responseIdentifier: fact.responseIdentifier }
        : {}),
    });
  } catch {
    return undefined;
  }
}

function validFact(fact: unknown): fact is Readonly<GoogletagDiagnosticsFact> {
  if (typeof fact !== 'object' || fact === null || !Object.isFrozen(fact)) return false;
  const kind = Object.getOwnPropertyDescriptor(fact, 'kind');
  const slot = Object.getOwnPropertyDescriptor(fact, 'slot');
  return (
    ((kind !== undefined &&
      'value' in kind &&
      DIAGNOSTICS_ONLY_EVENTS.includes(kind.value as (typeof DIAGNOSTICS_ONLY_EVENTS)[number])) ||
      kind?.value === 'slotRequested' ||
      kind?.value === 'slotRenderEnded') &&
    slot !== undefined &&
    'value' in slot &&
    ((typeof slot.value === 'object' && slot.value !== null) || typeof slot.value === 'function')
  );
}

const FIRST_DISPLAY_FACT_KEYS = Object.freeze([
  'version',
  'event',
  'token',
  'runtimeSlotNumber',
  'cycleOrdinal',
  'disposition',
  'issueReason',
  'capturedAtMs',
  'elementId',
  'adUnitPath',
  'isEmpty',
  'renderedSize',
  'isBackfill',
  'slotContentChanged',
  'visibilityPercent',
]);

function exactFrozenRecord(
  candidate: unknown,
  keys: readonly string[]
): Readonly<Record<string, unknown>> | undefined {
  if (
    typeof candidate !== 'object' ||
    candidate === null ||
    Array.isArray(candidate) ||
    !Object.isFrozen(candidate)
  ) {
    return undefined;
  }
  const actual = Reflect.ownKeys(candidate);
  if (
    actual.length !== keys.length ||
    actual.some((key) => typeof key !== 'string' || !keys.includes(key))
  ) {
    return undefined;
  }
  const result: Record<string, unknown> = {};
  for (const key of keys) {
    const descriptor = Object.getOwnPropertyDescriptor(candidate, key);
    if (!descriptor?.enumerable || !('value' in descriptor)) return undefined;
    result[key] = descriptor.value;
  }
  return result;
}

function validTransferredFact(candidate: unknown): candidate is Readonly<FirstDisplayGptFactV1> {
  const fields = exactFrozenRecord(candidate, FIRST_DISPLAY_FACT_KEYS);
  if (!fields) return false;
  const tokenOrdinal =
    typeof fields.token === 'string' && /^gt1_[1-9a-z][0-9a-z]{0,6}$/.test(fields.token)
      ? Number.parseInt(fields.token.slice(4), 36)
      : undefined;
  const event = fields.event;
  const disposition = fields.disposition;
  const issueReason = fields.issueReason;
  const renderedSize = fields.renderedSize;
  return (
    fields.version === 1 &&
    ALL_DIAGNOSTIC_EVENTS.has(event as string) &&
    tokenOrdinal !== undefined &&
    tokenOrdinal >= 1 &&
    tokenOrdinal <= 4_294_967_295 &&
    fields.runtimeSlotNumber === tokenOrdinal &&
    (fields.cycleOrdinal === null ||
      (Number.isInteger(fields.cycleOrdinal) &&
        (fields.cycleOrdinal as number) >= 1 &&
        (fields.cycleOrdinal as number) <= 4_294_967_295)) &&
    (disposition === 'matched' || disposition === 'unmatched' || disposition === 'ambiguous') &&
    (issueReason === null ||
      issueReason === 'no_request_cycle' ||
      issueReason === 'overlapping_request_cycles' ||
      issueReason === 'unknown_prior_cycle' ||
      issueReason === 'invalid_event_order') &&
    typeof fields.capturedAtMs === 'number' &&
    Number.isFinite(fields.capturedAtMs) &&
    fields.capturedAtMs >= 0 &&
    (fields.elementId === null ||
      (typeof fields.elementId === 'string' && fields.elementId.length > 0)) &&
    (fields.adUnitPath === null ||
      (typeof fields.adUnitPath === 'string' && fields.adUnitPath.length > 0)) &&
    (fields.isEmpty === null || typeof fields.isEmpty === 'boolean') &&
    (renderedSize === null ||
      (Array.isArray(renderedSize) &&
        Object.isFrozen(renderedSize) &&
        renderedSize.length === 2 &&
        renderedSize.every(
          (dimension) => Number.isInteger(dimension) && dimension >= 1 && dimension <= 4096
        ))) &&
    (fields.isBackfill === null || typeof fields.isBackfill === 'boolean') &&
    (fields.slotContentChanged === null || typeof fields.slotContentChanged === 'boolean') &&
    (fields.visibilityPercent === null ||
      (typeof fields.visibilityPercent === 'number' &&
        Number.isFinite(fields.visibilityPercent) &&
        fields.visibilityPercent >= 0 &&
        fields.visibilityPercent <= 100))
  );
}

function rawFirstDisplayFacts(
  diagnostics: Readonly<FirstDisplayGptDiagnosticsV1>,
  resolveSlot?: (traceToken: string) => Readonly<GoogletagDiagnosticsSlotSnapshot> | undefined
): readonly Readonly<GoogletagDiagnosticsFact>[] | undefined {
  const fields = exactFrozenRecord(diagnostics, ['facts', 'overflowCount', 'dropCount']);
  if (
    !fields ||
    !Array.isArray(fields.facts) ||
    !Object.isFrozen(fields.facts) ||
    fields.facts.length > MAX_BUFFERED_FACTS ||
    !Number.isInteger(fields.overflowCount) ||
    (fields.overflowCount as number) < 0 ||
    (fields.overflowCount as number) > 4_294_967_295 ||
    !Number.isInteger(fields.dropCount) ||
    (fields.dropCount as number) < 0 ||
    (fields.dropCount as number) > 4_294_967_295
  ) {
    return undefined;
  }
  const slotByTraceToken = new Map<string, Readonly<GoogletagDiagnosticsSlotSnapshot>>();
  const facts: Array<Readonly<GoogletagDiagnosticsFact>> = [];
  for (const candidate of fields.facts) {
    if (!validTransferredFact(candidate)) return undefined;
    let slot = slotByTraceToken.get(candidate.token);
    if (!slot) {
      let resolved: Readonly<GoogletagDiagnosticsSlotSnapshot> | undefined;
      try {
        resolved = resolveSlot?.(candidate.token);
      } catch {
        return undefined;
      }
      if (
        resolved &&
        (typeof resolved.token !== 'object' ||
          resolved.token === null ||
          resolved.traceToken !== candidate.token ||
          resolved.runtimeSlotNumber !== candidate.runtimeSlotNumber)
      ) {
        return undefined;
      }
      slot = Object.freeze({
        token: resolved?.token ?? Object.freeze(Object.create(null) as object),
        traceToken: candidate.token as GptSlotTokenV1,
        runtimeSlotNumber: candidate.runtimeSlotNumber,
        ...(candidate.cycleOrdinal === null
          ? {}
          : { cycleOrdinal: candidate.cycleOrdinal as GptTraceCycleOrdinalV1 }),
        ...(candidate.elementId === null ? {} : { elementId: candidate.elementId }),
        ...(candidate.adUnitPath === null ? {} : { adUnitPath: candidate.adUnitPath }),
      });
      slotByTraceToken.set(candidate.token, slot);
    }
    facts.push(
      Object.freeze({
        kind: candidate.event,
        observedAtMs: candidate.capturedAtMs,
        slot,
        ...(candidate.isEmpty === null ? {} : { isEmpty: candidate.isEmpty }),
        ...(candidate.renderedSize === null ? {} : { size: candidate.renderedSize }),
        ...(candidate.isBackfill === null ? {} : { isBackfill: candidate.isBackfill }),
        ...(candidate.slotContentChanged === null
          ? {}
          : { slotContentChanged: candidate.slotContentChanged }),
        ...(candidate.visibilityPercent === null
          ? {}
          : { inViewPercentage: candidate.visibilityPercent }),
      })
    );
  }
  return Object.freeze(facts);
}

/** Own the GPT-side bounded handoff between early callbacks and diagnostics consumers. */
export function createGptDiagnosticsFactBuffer(
  options: GptDiagnosticsFactBufferOptions = {}
): GptDiagnosticsFactBuffer {
  const pending: Readonly<GoogletagDiagnosticsFact>[] = [];
  let consumer: GoogletagDiagnosticsObserver | undefined;
  let consumerGeneration = 0;
  let replaying = false;
  let disposed = false;
  let droppedFacts = 0;
  let adoptionOpen = true;

  const reportConsumerError = (error: unknown): void => {
    try {
      options.onConsumerError?.(error);
    } catch {
      // Diagnostics error reporting cannot affect later fact delivery.
    }
  };
  const reportOverflow = (): void => {
    droppedFacts += 1;
    try {
      options.onOverflow?.(droppedFacts);
    } catch {
      // Overflow reporting is diagnostics-only.
    }
  };
  const enqueue = (fact: Readonly<GoogletagDiagnosticsFact>): void => {
    if (pending.length >= MAX_BUFFERED_FACTS) {
      pending.shift();
      reportOverflow();
    }
    pending.push(fact);
  };
  const deliver = (fact: Readonly<GoogletagDiagnosticsFact>): void => {
    const current = consumer;
    if (!current) return;
    try {
      current(fact);
    } catch (error) {
      reportConsumerError(error);
    }
  };

  return Object.freeze({
    adoptFirstDisplay: (
      diagnostics: Readonly<FirstDisplayGptDiagnosticsV1>,
      resolveSlot?: (traceToken: string) => Readonly<GoogletagDiagnosticsSlotSnapshot> | undefined
    ): boolean => {
      const facts = rawFirstDisplayFacts(diagnostics, resolveSlot);
      if (disposed || !adoptionOpen || pending.length !== 0 || consumer !== undefined || !facts) {
        return false;
      }
      pending.push(...facts);
      droppedFacts = diagnostics.overflowCount;
      adoptionOpen = false;
      return true;
    },
    publish: (fact: Readonly<GoogletagDiagnosticsFact>): boolean => {
      adoptionOpen = false;
      if (disposed || !validFact(fact)) return false;
      if (replaying || !consumer) enqueue(fact);
      else deliver(fact);
      return true;
    },
    activate: (nextConsumer: GoogletagDiagnosticsObserver): (() => void) | undefined => {
      adoptionOpen = false;
      if (disposed || consumer || typeof nextConsumer !== 'function') return undefined;
      consumer = nextConsumer;
      consumerGeneration += 1;
      const generation = consumerGeneration;
      replaying = true;
      while (pending.length > 0 && consumer === nextConsumer && !disposed) {
        const fact = pending.shift();
        if (fact) deliver(fact);
      }
      replaying = false;
      if (!consumer || disposed) pending.length = 0;
      let active = true;
      return (): void => {
        if (!active) return;
        active = false;
        if (consumerGeneration === generation && consumer === nextConsumer) consumer = undefined;
      };
    },
    dispose: (): void => {
      if (disposed) return;
      disposed = true;
      consumer = undefined;
      replaying = false;
      pending.length = 0;
    },
  });
}

function activateEventListeners(
  adapter: Pick<GoogletagAdapter, 'run'>,
  onFailure: () => void = () => undefined
): (() => void) | undefined {
  let disposed = false;
  let releases: readonly (() => void)[] = Object.freeze([]);
  let operation: ReturnType<GoogletagAdapter['run']> | undefined;
  try {
    operation = adapter.run((gpt) => {
      const installed: Array<() => void> = [];
      try {
        for (let index = 0; index < DIAGNOSTICS_ONLY_EVENTS.length; index += 1) {
          const eventType = DIAGNOSTICS_ONLY_EVENTS[index];
          if (!eventType) continue;
          installed[installed.length] = gpt.subscribe(eventType, () => undefined, true);
        }
        return Object.freeze(installed);
      } catch (error) {
        for (let index = installed.length - 1; index >= 0; index -= 1) {
          try {
            installed[index]?.();
          } catch {
            // Continue rolling back the remaining listener ownership.
          }
        }
        throw error;
      }
    });
    void operation.result.then(
      (installed) => {
        releases = installed as readonly (() => void)[];
        if (!disposed) return;
        for (let index = releases.length - 1; index >= 0; index -= 1) {
          try {
            releases[index]?.();
          } catch {
            // Late completion still releases every listener independently.
          }
        }
        releases = Object.freeze([]);
      },
      () => {
        try {
          onFailure();
        } catch {
          // Failure notification cannot retain listener ownership.
        }
      }
    );
  } catch {
    onFailure();
    return undefined;
  }

  return (): void => {
    if (disposed) return;
    disposed = true;
    try {
      operation?.dispose();
    } catch {
      // Disposal continues through every independently owned resource.
    }
    for (let index = releases.length - 1; index >= 0; index -= 1) {
      try {
        releases[index]?.();
      } catch {
        // One hostile listener release cannot retain the others.
      }
    }
    releases = Object.freeze([]);
  };
}

/** Install only the four GPT listeners that exist while diagnostics is active. */
export function activateGptDiagnosticsEventListeners(
  adapter: Pick<GoogletagAdapter, 'run'>
): (() => void) | undefined {
  return activateEventListeners(adapter);
}

/** Connect the sole GPT adapter stream and only the four diagnostics-only listeners. */
export function activateGptDiagnosticsFactCapture(
  adapter: Pick<GoogletagAdapter, 'observeDiagnostics' | 'run'>,
  buffer: Pick<GptDiagnosticsFactBuffer, 'publish'>
): (() => void) | undefined {
  let disposed = false;
  const observerRelease = adapter.observeDiagnostics((fact) => {
    try {
      buffer.publish(fact);
    } catch {
      // Fact buffering cannot alter the already-completed GPT callback.
    }
  });
  if (!observerRelease) return undefined;
  let observerActive = true;
  const releaseObserver = (): void => {
    if (!observerActive) return;
    observerActive = false;
    observerRelease();
  };
  const releaseListeners = activateEventListeners(adapter, releaseObserver);
  if (!releaseListeners) {
    releaseObserver();
    return undefined;
  }
  return (): void => {
    if (disposed) return;
    disposed = true;
    try {
      releaseListeners();
    } finally {
      releaseObserver();
    }
  };
}
