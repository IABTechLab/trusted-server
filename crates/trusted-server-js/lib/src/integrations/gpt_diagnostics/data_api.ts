import type {
  GptDiagnosticsApi,
  GptDiagnosticsBinding,
  GptDiagnosticsExportV1,
} from '../../core/types';
import { DiagnosticsSubscriberLimitError } from '../../core/trace';

import type { GptDiagnosticsBindingInput, GptDiagnosticsStoreSnapshot } from './store';

interface DiagnosticsDataStore {
  readonly bindingInputs: () => GptDiagnosticsBindingInput[];
  readonly snapshot: () => GptDiagnosticsStoreSnapshot;
  readonly subscribe: (listener: () => void) => () => void;
  readonly subscribeCommits: (listener: () => void) => () => void;
}

export interface GptDiagnosticsPresentationControls {
  readonly dispose: () => void;
  readonly download: () => void;
  readonly exportBinding: (runtimeSlotNumber: number) => GptDiagnosticsBinding;
  readonly hide: () => void;
  readonly show: () => void;
  readonly subscribe: (listener: () => void) => () => void;
}

export interface GptDiagnosticsPresentationSource {
  readonly bindingInputs: () => GptDiagnosticsBindingInput[];
  readonly snapshot: () => GptDiagnosticsStoreSnapshot;
  readonly subscribe: (listener: () => void) => () => void;
}

export type GptDiagnosticsPresentationFactory = (
  source: GptDiagnosticsPresentationSource,
  api: GptDiagnosticsApi
) => GptDiagnosticsPresentationControls;

interface DataApiOptions {
  readonly location: Pick<Location, 'origin' | 'pathname'>;
  readonly now?: (() => Date) | undefined;
  readonly schedule?: ((callback: () => void) => () => void) | undefined;
}

type ApiListener = (snapshot: GptDiagnosticsExportV1) => void;

interface PendingNotification {
  readonly snapshot: GptDiagnosticsExportV1;
  readonly subscriberIds: readonly number[];
}

const MAX_API_SUBSCRIBERS = 32;

function scheduleTask(callback: () => void): () => void {
  const handle = globalThis.setTimeout(callback, 0);
  return () => globalThis.clearTimeout(handle);
}

function defaultBinding(): GptDiagnosticsBinding {
  return Object.freeze({ status: 'unbound', reason: 'missing_element' });
}

function isolate(callback: () => void): void {
  try {
    callback();
  } catch {
    // Presentation cleanup cannot retain another independently owned resource.
  }
}

function validPresentationControls(
  candidate: unknown
): candidate is GptDiagnosticsPresentationControls {
  try {
    if (typeof candidate !== 'object' || candidate === null || !Object.isFrozen(candidate)) {
      return false;
    }
    const expected = ['dispose', 'download', 'exportBinding', 'hide', 'show', 'subscribe'];
    const keys = Reflect.ownKeys(candidate);
    if (keys.length !== expected.length || keys.some((key) => typeof key !== 'string'))
      return false;
    for (let index = 0; index < expected.length; index += 1) {
      const descriptor = Object.getOwnPropertyDescriptor(candidate, expected[index] as string);
      if (!descriptor || !('value' in descriptor) || typeof descriptor.value !== 'function') {
        return false;
      }
    }
    return keys.every((key) => expected.includes(key as string));
  } catch {
    return false;
  }
}

function disposePresentationCandidate(candidate: unknown): void {
  try {
    if (typeof candidate !== 'object' || candidate === null) return;
    const descriptor = Object.getOwnPropertyDescriptor(candidate, 'dispose');
    if (descriptor && 'value' in descriptor && typeof descriptor.value === 'function') {
      Reflect.apply(descriptor.value, candidate, []);
    }
  } catch {
    // A malformed presentation cannot block its own rollback boundary.
  }
}

/** Owns the stable, data-only GPT diagnostics API published in the takeover barrier. */
export class GptDiagnosticsDataApiController {
  readonly api: GptDiagnosticsApi;

  private readonly store: DiagnosticsDataStore;
  private readonly location: Pick<Location, 'origin' | 'pathname'>;
  private readonly now: () => Date;
  private readonly schedule: (callback: () => void) => () => void;
  private readonly listeners = new Map<number, ApiListener>();
  private readonly unsubscribeStore: () => void;
  private readonly presentationSource: GptDiagnosticsPresentationSource;
  private presentation: GptDiagnosticsPresentationControls | undefined;
  private unsubscribePresentation: (() => void) | undefined;
  private pending: PendingNotification | undefined;
  private cancelScheduled: (() => void) | undefined;
  private nextSubscriberId = 0;
  private presentationAttaching = false;
  private destroyed = false;

  constructor(store: DiagnosticsDataStore, options: DataApiOptions) {
    this.store = store;
    this.location = options.location;
    this.now = options.now ?? (() => new Date());
    this.schedule = options.schedule ?? scheduleTask;
    this.unsubscribeStore = store.subscribeCommits(() => this.scheduleNotification());
    this.presentationSource = Object.freeze({
      bindingInputs: () => store.bindingInputs(),
      snapshot: () => store.snapshot(),
      subscribe: (listener: () => void) => store.subscribe(listener),
    });
    this.api = Object.freeze({
      snapshot: () => this.snapshot(),
      export: () => this.presentation?.download(),
      subscribe: (listener: ApiListener) => this.subscribe(listener),
      show: () => this.presentation?.show(),
      hide: () => this.presentation?.hide(),
    });
  }

  attachPresentation(factory: GptDiagnosticsPresentationFactory): () => void {
    if (typeof factory !== 'function') {
      throw new TypeError('GPT diagnostics presentation factory must be callable');
    }
    if (this.destroyed || this.presentation || this.presentationAttaching) {
      throw new TypeError('GPT diagnostics presentation is unavailable');
    }
    this.presentationAttaching = true;
    let candidate: unknown;
    try {
      candidate = factory(this.presentationSource, this.api);
      if (!validPresentationControls(candidate)) {
        throw new TypeError('GPT diagnostics presentation controls are malformed');
      }
      const controls = candidate;
      const release = controls.subscribe(() => this.scheduleNotification());
      if (typeof release !== 'function') {
        throw new TypeError('GPT diagnostics presentation disposer is unavailable');
      }
      this.presentation = controls;
      this.unsubscribePresentation = release;
      let active = true;
      return (): void => {
        if (!active) return;
        active = false;
        if (this.presentation !== controls) return;
        this.presentation = undefined;
        this.unsubscribePresentation = undefined;
        isolate(release);
        isolate(() => controls.dispose());
      };
    } catch (error) {
      disposePresentationCandidate(candidate);
      throw error;
    } finally {
      this.presentationAttaching = false;
    }
  }

  snapshot(): GptDiagnosticsExportV1 {
    const store = this.store.snapshot();
    const presentation = this.presentation;
    return Object.freeze({
      version: 1,
      capturedAt: this.now().toISOString(),
      page: Object.freeze({
        origin: this.location.origin,
        pathname: this.location.pathname,
      }),
      slots: Object.freeze(
        store.slots.map((slot) =>
          Object.freeze({
            runtimeSlotNumber: slot.runtimeSlotNumber,
            slotElementId: slot.slotElementId,
            adUnitPath: slot.adUnitPath,
            binding: Object.freeze(
              presentation?.exportBinding(slot.runtimeSlotNumber) ?? defaultBinding()
            ),
            currentVisibilityPercentage: slot.currentVisibilityPercentage,
            maximumVisibilityPercentage: slot.maximumVisibilityPercentage,
            requests: Object.freeze(
              slot.requests.map((cycle) =>
                Object.freeze({
                  ...cycle,
                  durations: Object.freeze({ ...cycle.durations }),
                  requestedSlotSizes: cycle.requestedSlotSizes
                    ? Object.freeze(
                        cycle.requestedSlotSizes.map((size) => Object.freeze([...size]))
                      )
                    : undefined,
                  size: cycle.size ? Object.freeze([...cycle.size]) : undefined,
                  observedSlotSize: cycle.observedSlotSize
                    ? Object.freeze([...cycle.observedSlotSize])
                    : undefined,
                  adManager: cycle.adManager
                    ? Object.freeze({
                        ...cycle.adManager,
                        yieldGroupIds: cycle.adManager.yieldGroupIds
                          ? Object.freeze([...cycle.adManager.yieldGroupIds])
                          : undefined,
                        companyIds: cycle.adManager.companyIds
                          ? Object.freeze([...cycle.adManager.companyIds])
                          : undefined,
                      })
                    : undefined,
                  trustedServerCreativeFailures: cycle.trustedServerCreativeFailures
                    ? Object.freeze([...cycle.trustedServerCreativeFailures])
                    : undefined,
                })
              )
            ),
          })
        )
      ),
      callbackIssues: Object.freeze(
        store.callbackIssues.map((issue) => Object.freeze({ ...issue }))
      ),
      attributionIssues: Object.freeze(
        store.attributionIssues.map((issue) => Object.freeze({ ...issue }))
      ),
      coverage: Object.freeze(
        Object.fromEntries(
          Object.entries(store.coverage).map(([kind, counters]) => [
            kind,
            Object.freeze({ ...counters }),
          ])
        )
      ) as GptDiagnosticsExportV1['coverage'],
      metadata: Object.freeze({ ...store.metadata }),
    }) as GptDiagnosticsExportV1;
  }

  destroy(): void {
    if (this.destroyed) return;
    this.destroyed = true;
    isolate(this.unsubscribeStore);
    const releasePresentation = this.unsubscribePresentation;
    const presentation = this.presentation;
    this.unsubscribePresentation = undefined;
    this.presentation = undefined;
    if (releasePresentation) isolate(releasePresentation);
    if (presentation) isolate(() => presentation.dispose());
    if (this.cancelScheduled) isolate(this.cancelScheduled);
    this.cancelScheduled = undefined;
    this.pending = undefined;
    this.listeners.clear();
  }

  private subscribe(listener: ApiListener): () => void {
    if (typeof listener !== 'function') {
      throw new TypeError('Diagnostics listener must be callable');
    }
    if (this.destroyed) return () => undefined;
    if (this.listeners.size >= MAX_API_SUBSCRIBERS) {
      throw new DiagnosticsSubscriberLimitError('gpt');
    }
    const id = (this.nextSubscriberId += 1);
    this.listeners.set(id, listener);
    let active = true;
    return (): void => {
      if (!active) return;
      active = false;
      this.listeners.delete(id);
    };
  }

  private scheduleNotification(): void {
    if (this.destroyed || this.listeners.size === 0) return;
    this.pending = Object.freeze({
      snapshot: this.snapshot(),
      subscriberIds: Object.freeze([...this.listeners.keys()]),
    });
    if (this.cancelScheduled) return;
    try {
      const cancel = this.schedule(() => {
        this.cancelScheduled = undefined;
        const notification = this.pending;
        this.pending = undefined;
        if (this.destroyed || !notification) return;
        for (const id of notification.subscriberIds) {
          const listener = this.listeners.get(id);
          if (!listener) continue;
          try {
            listener(notification.snapshot);
          } catch {
            // One public diagnostics subscriber cannot block the remaining listeners.
          }
        }
      });
      if (typeof cancel !== 'function') throw new TypeError('Invalid diagnostics scheduler');
      if (!this.destroyed && this.pending) this.cancelScheduled = cancel;
    } catch {
      this.cancelScheduled = undefined;
      this.pending = undefined;
    }
  }
}
