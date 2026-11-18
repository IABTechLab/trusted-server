import { log } from '../../core/log';

/** One settled TCF decision, reduced to what the EC route client needs. */
export interface TcfReading {
  gdprApplies: boolean;
  tcString: string;
  /** Sorted true-valued grants: `p<id>`/`pl<id>` purposes, `v<id>`/`vl<id>` vendors. */
  granted: string[];
}

type TcfCallback = (data: unknown, success: boolean) => void;
type TcfApi = (
  command: string,
  version: number,
  callback: TcfCallback,
  parameter?: unknown
) => void;
type TcfWindow = Window & { __tcfapi?: unknown };

const TERMINAL_EVENT_STATUSES = ['tcloaded', 'useractioncomplete'];

function fields(value: unknown): Record<string, unknown> {
  return value !== null && typeof value === 'object' && !Array.isArray(value)
    ? (value as Record<string, unknown>)
    : {};
}

function trueKeys(map: unknown, prefix: string): string[] {
  return Object.entries(fields(map))
    .filter(([, value]) => value === true)
    .map(([key]) => `${prefix}${key}`);
}

function toReading(data: unknown): TcfReading | undefined {
  const tcData = fields(data);
  if (typeof tcData.gdprApplies !== 'boolean') return undefined;
  if (
    tcData.gdprApplies &&
    !(
      typeof tcData.eventStatus === 'string' && TERMINAL_EVENT_STATUSES.includes(tcData.eventStatus)
    )
  ) {
    return undefined;
  }
  const purpose = fields(tcData.purpose);
  const vendor = fields(tcData.vendor);
  return {
    gdprApplies: tcData.gdprApplies,
    tcString: typeof tcData.tcString === 'string' ? tcData.tcString : '',
    granted: [
      ...trueKeys(purpose.consents, 'p'),
      ...trueKeys(purpose.legitimateInterests, 'pl'),
      ...trueKeys(vendor.consents, 'v'),
      ...trueKeys(vendor.legitimateInterests, 'vl'),
    ].sort(),
  };
}

/**
 * Observes the publisher's existing TCF v2 CMP. It never installs a CMP, stub
 * or accessor; callers re-run {@link TcfReader.refresh} to discover a late or
 * replaced `__tcfapi`.
 */
export class TcfReader {
  private api?: TcfApi;
  private listenerId?: unknown;
  private generation = 0;
  private stopped = false;

  constructor(
    private readonly target: TcfWindow,
    private readonly onReading: (reading: TcfReading) => void
  ) {}

  /** Subscribes to a newly published or replaced CMP; an unchanged one is left alone. */
  refresh(): void {
    if (this.stopped) return;
    const next = this.target.__tcfapi;
    if (typeof next !== 'function' || next === this.api) return;
    this.unsubscribe();
    this.api = next as TcfApi;
    const generation = this.generation;
    const api = this.api;
    let staleRemoved = false;
    const onEvent: TcfCallback = (data, success) => {
      const listenerId = fields(data).listenerId;
      const hasId = listenerId !== undefined && listenerId !== null;
      if (this.stopped || generation !== this.generation) {
        // A stub hands queued listeners to the real CMP, so a superseded
        // subscription can fire with an id it never reported before. The
        // drained stub no longer reaches the CMP that now holds the listener,
        // so remove it through the current API, and through the accepting API
        // when that is a different, still-callable CMP. Once.
        if (hasId && !staleRemoved) {
          staleRemoved = true;
          const current = this.target.__tcfapi;
          for (const owner of new Set([current, api])) {
            if (typeof owner !== 'function') continue;
            try {
              (owner as TcfApi).call(this.target, 'removeEventListener', 2, () => {}, listenerId);
            } catch (error) {
              log.warn('[tsjs-prebid] stale TCF consent listener cleanup failed', error);
            }
          }
        }
        return;
      }
      if (hasId) this.listenerId = listenerId;
      if (success !== true) return;
      const reading = toReading(data);
      if (reading) this.onReading(reading);
    };
    try {
      api.call(this.target, 'addEventListener', 2, onEvent);
    } catch (error) {
      log.warn('[tsjs-prebid] TCF consent subscription failed', error);
    }
  }

  /** Removes the subscription from the API that accepted it. */
  stop(): void {
    this.unsubscribe();
    this.stopped = true;
  }

  private unsubscribe(): void {
    this.generation++;
    const { api, listenerId } = this;
    this.listenerId = undefined;
    if (!api || listenerId === undefined) return;
    try {
      api.call(this.target, 'removeEventListener', 2, () => {}, listenerId);
    } catch (error) {
      log.warn('[tsjs-prebid] TCF consent listener cleanup failed', error);
    }
  }
}
