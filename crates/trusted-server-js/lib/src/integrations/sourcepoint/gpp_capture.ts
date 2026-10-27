import { log } from '../../core/log';

export interface GppConsent {
  gppString: string;
  applicableSections: number[];
}

type GppCallback = (data: unknown, success?: boolean) => void;
export type GppApi = (command: string, callback: GppCallback, parameter?: number) => unknown;
export type GppWindow = Window & { __gpp?: GppApi };

/** GPP's encoded alphabet, safe as one cookie value without truncation. */
export function isCookieSafeGppString(value: unknown): value is string {
  return typeof value === 'string' && value.length > 0 && !/[^A-Za-z0-9._~=-]/.test(value);
}

function record(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === 'object' && !Array.isArray(value);
}

/** Observes the existing CMP API. It does not install a CMP or synthesize consent. */
export class GppConsentCapture {
  private api?: GppApi;
  private listenerId?: number;
  private registrationPending = false;
  private generation = 0;
  private snapshotEpoch = 0;
  private stopped = false;
  private restoreProperty?: () => void;
  private readonly onScriptLoaded = () => {
    if (this.target.__gpp !== this.api) this.refresh();
  };

  constructor(
    private readonly target: GppWindow,
    private readonly onConsent: (consent: GppConsent) => void,
    private readonly onUnavailable: () => void
  ) {}

  start(): void {
    if (this.stopped) return;
    const original = Object.getOwnPropertyDescriptor(this.target, '__gpp');
    // A plain writable global is how the standard CMP stub publishes its API.
    // Preserve custom/non-configurable accessors rather than changing their semantics.
    if (!original || (original.configurable && 'value' in original && original.writable)) {
      let published = this.target.__gpp;
      const getter = (): GppApi | undefined => published;
      Object.defineProperty(this.target, '__gpp', {
        configurable: true,
        enumerable: original?.enumerable ?? true,
        get: getter,
        set: (value: GppApi | undefined) => {
          published = value;
          this.refresh();
        },
      });
      this.restoreProperty = () => {
        if (Object.getOwnPropertyDescriptor(this.target, '__gpp')?.get !== getter) return;
        if (!original && published === undefined) {
          delete this.target.__gpp;
        } else {
          Object.defineProperty(this.target, '__gpp', {
            configurable: original?.configurable ?? true,
            enumerable: original?.enumerable ?? true,
            writable: original && 'writable' in original ? original.writable : true,
            value: published,
          });
        }
      };
    }
    // Resource completion also observes SDK publication through a descriptor we
    // deliberately did not replace. No timer/polling or synthetic GPP stub.
    this.target.document.addEventListener('load', this.onScriptLoaded, true);
    this.target.document.addEventListener('DOMContentLoaded', this.onScriptLoaded);
    this.refresh();
  }

  hasApi(): boolean {
    return typeof this.target.__gpp === 'function';
  }

  refresh(): void {
    if (this.stopped) return;
    const next = this.target.__gpp;
    if (typeof next !== 'function') {
      if (this.api) {
        this.unsubscribe();
        this.api = undefined;
        this.onUnavailable();
      }
      return;
    }
    if (next !== this.api) {
      // A queued stub subscription transfers to the real CMP. Registering again
      // while it is pending would leave two listeners after the queue drains.
      if (!this.registrationPending) this.unsubscribe();
      this.api = next;
    }
    this.ping(next);
  }

  stop(): void {
    if (this.stopped) return;
    this.stopped = true;
    this.unsubscribe();
    this.target.document.removeEventListener('load', this.onScriptLoaded, true);
    this.target.document.removeEventListener('DOMContentLoaded', this.onScriptLoaded);
    this.restoreProperty?.();
  }

  private subscribe(api: GppApi, version: '1.0' | '1.1'): void {
    this.registrationPending = true;
    const generation = this.generation;
    const onEvent = (event: unknown, success?: boolean) => {
      if (this.stopped || generation !== this.generation) return;
      this.snapshotEpoch += 1;
      if ((success !== true && !(version === '1.0' && success === undefined)) || !record(event)) {
        this.unsubscribe();
        this.onUnavailable();
        return;
      }
      if (event.eventName === 'listenerRegistered') {
        if (
          event.data !== true ||
          typeof event.listenerId !== 'number' ||
          !Number.isInteger(event.listenerId)
        ) {
          this.unsubscribe();
          this.onUnavailable();
          return;
        }
        this.listenerId = event.listenerId;
        this.registrationPending = false;
      }
      if (this.listenerId === undefined) return;
      // Event pingData and outstanding asynchronous reads can predate a newer
      // choice. Read current state and discard superseded ping callbacks.
      if (this.api) this.ping(this.api);
    };
    try {
      const returned = api.call(this.target, 'addEventListener', onEvent);
      // The published 1.1 example stub still returns this acknowledgment
      // synchronously. Accept registration only, never synchronous 1.1 consent.
      if (
        this.listenerId === undefined &&
        record(returned) &&
        returned.eventName === 'listenerRegistered'
      ) {
        onEvent(returned, true);
      }
    } catch {
      this.unsubscribe();
      this.onUnavailable();
      log.warn('sourcepoint: GPP event subscription failed');
    }
  }

  private ping(api: GppApi): void {
    const epoch = ++this.snapshotEpoch;
    let callbackCalled = false;
    const onPing = (data: unknown, success?: boolean) => {
      callbackCalled = true;
      if (this.stopped || api !== this.api || epoch !== this.snapshotEpoch) return;
      if (!success || !record(data) || (data.gppVersion !== '1.0' && data.gppVersion !== '1.1')) {
        this.onUnavailable();
        return;
      }
      if (this.listenerId === undefined) {
        if (!this.registrationPending) this.subscribe(api, data.gppVersion);
        if (this.listenerId === undefined) this.onUnavailable();
        return;
      }
      if (data.gppVersion === '1.0') {
        // API 1.0 has synchronous getGPPData, not the 1.1 ready ping snapshot.
        if (data.cmpStatus !== 'loaded') {
          this.onUnavailable();
          return;
        }
        const legacy = api.call(this.target, 'getGPPData', () => {});
        if (record(legacy)) this.consume(legacy);
        else this.onUnavailable();
      } else if (data.signalStatus === 'ready') this.consume(data);
      else this.onUnavailable();
    };
    try {
      const returned = api.call(this.target, 'ping', onPing);
      if (!callbackCalled && record(returned)) {
        if (returned.gppVersion === '1.0') onPing(returned, true);
        else if (returned.gppVersion === '1.1' && epoch === this.snapshotEpoch) {
          this.onUnavailable();
          if (this.listenerId === undefined && !this.registrationPending)
            this.subscribe(api, '1.1');
        }
      }
    } catch {
      this.onUnavailable();
      log.warn('sourcepoint: GPP state read failed');
    }
  }

  private consume(data: Record<string, unknown>): void {
    if (
      !isCookieSafeGppString(data.gppString) ||
      !Array.isArray(data.applicableSections) ||
      data.applicableSections.length === 0 ||
      !data.applicableSections.every((id) => Number.isInteger(id) && id > 0 && id <= 65535)
    ) {
      this.onUnavailable();
      return;
    }
    this.onConsent({
      gppString: data.gppString,
      applicableSections: [...data.applicableSections] as number[],
    });
  }

  private unsubscribe(): void {
    this.generation++;
    this.snapshotEpoch++;
    if (this.listenerId !== undefined && this.api) {
      try {
        this.api.call(this.target, 'removeEventListener', () => {}, this.listenerId);
      } catch {
        log.warn('sourcepoint: GPP listener cleanup failed');
      }
    }
    this.listenerId = undefined;
    this.registrationPending = false;
  }
}
