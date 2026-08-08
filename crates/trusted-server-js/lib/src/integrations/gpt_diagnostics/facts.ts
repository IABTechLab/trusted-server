import type {
  GoogletagAdapter,
  GoogletagDiagnosticsFact,
  GoogletagDiagnosticsObserver,
} from '../../adapters/googletag';

const MAX_BUFFERED_FACTS = 512;
const DIAGNOSTICS_ONLY_EVENTS = Object.freeze([
  'slotResponseReceived',
  'slotOnload',
  'impressionViewable',
  'slotVisibilityChanged',
] as const);

export interface GptDiagnosticsFactBufferOptions {
  readonly onConsumerError?: (error: unknown) => void;
  readonly onOverflow?: (droppedFacts: number) => void;
}

export interface GptDiagnosticsFactBuffer {
  readonly publish: (fact: Readonly<GoogletagDiagnosticsFact>) => boolean;
  readonly activate: (consumer: GoogletagDiagnosticsObserver) => (() => void) | undefined;
  readonly dispose: () => void;
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

/** Own the bounded handoff between early GPT callbacks and the diagnostics module. */
export function createGptDiagnosticsFactBuffer(
  options: GptDiagnosticsFactBufferOptions = {}
): GptDiagnosticsFactBuffer {
  const pending: Readonly<GoogletagDiagnosticsFact>[] = [];
  let consumer: GoogletagDiagnosticsObserver | undefined;
  let consumerGeneration = 0;
  let replaying = false;
  let disposed = false;
  let droppedFacts = 0;

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
    publish: (fact: Readonly<GoogletagDiagnosticsFact>): boolean => {
      if (disposed || !validFact(fact)) return false;
      if (replaying || !consumer) enqueue(fact);
      else deliver(fact);
      return true;
    },
    activate: (nextConsumer: GoogletagDiagnosticsObserver): (() => void) | undefined => {
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

/** Connect the sole GPT adapter stream and only the four diagnostics-only listeners. */
export function activateGptDiagnosticsFactCapture(
  adapter: Pick<GoogletagAdapter, 'observeDiagnostics' | 'run'>,
  buffer: Pick<GptDiagnosticsFactBuffer, 'publish'>
): (() => void) | undefined {
  let disposed = false;
  let releases: readonly (() => void)[] = Object.freeze([]);
  const observeDiagnostics = adapter.observeDiagnostics;
  const releaseObserver = observeDiagnostics((fact) => {
    try {
      buffer.publish(fact);
    } catch {
      // Fact buffering cannot alter the already-completed GPT callback.
    }
  });
  if (!releaseObserver) return undefined;

  let operation: ReturnType<GoogletagAdapter['run']> | undefined;
  try {
    operation = adapter.run((gpt) => {
      const installed: Array<() => void> = [];
      try {
        for (let index = 0; index < DIAGNOSTICS_ONLY_EVENTS.length; index += 1) {
          const eventType = DIAGNOSTICS_ONLY_EVENTS[index];
          if (!eventType) continue;
          installed[installed.length] = gpt.subscribe(eventType, () => undefined);
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
          releaseObserver();
        } catch {
          // Failed activation retains no observer ownership.
        }
      }
    );
  } catch {
    releaseObserver();
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
    try {
      releaseObserver();
    } catch {
      // The adapter's disposed latch remains authoritative.
    }
  };
}
