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

  constructor(
    private readonly target: TcfWindow,
    private readonly onReading: (reading: TcfReading) => void
  ) {}

  /** Subscribes to a newly published or replaced CMP; an unchanged one is left alone. */
  refresh(): void {
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
      if (generation !== this.generation) {
        // Listener IDs belong to the API that accepted the subscription.
        // A replacement CMP may reuse the same ID for its active listener.
        // An unreachable stub listener stays inert behind the generation guard.
        if (hasId && !staleRemoved) {
          staleRemoved = true;
          try {
            api.call(this.target, 'removeEventListener', 2, () => {}, listenerId);
          } catch (error) {
            log.warn('[tsjs-prebid] stale TCF consent listener cleanup failed', error);
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
