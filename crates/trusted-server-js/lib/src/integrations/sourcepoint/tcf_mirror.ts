import { log } from '../../core/log';

const COOKIE = '__Host-ts-tcf';
const CMP_COOKIE = 'euconsent-v2';
const BASE64URL = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_';

interface TcfData {
  cmpStatus?: string;
  eventStatus?: string;
  gdprApplies?: boolean;
  listenerId?: number;
  tcString?: string;
  purpose?: { consents?: Record<number, boolean> };
}

type TcfApi = (
  command: 'addEventListener' | 'removeEventListener',
  version: 2,
  callback: (data: TcfData | null, success: boolean) => void,
  listenerId?: number
) => void;

type TcfWindow = Window & { __tcfapi?: TcfApi };
interface TcfSubscription {
  api: TcfApi;
  listenerId?: number;
}

function readCookie(doc: Document, name: string): string | undefined {
  const prefix = `${name}=`;
  return doc.cookie
    .split(';')
    .map((pair) => pair.trim())
    .find((pair) => pair.startsWith(prefix))
    ?.slice(prefix.length);
}

function lastUpdated(value: string): number | undefined {
  if (value.length > 3600 || !/^C[A-Za-z0-9_-]{38,}(?:\.[A-Za-z0-9_-]+)*$/.test(value)) {
    return undefined;
  }
  // The TCF v2 core segment begins with Version (6 bits), Created (36),
  // then LastUpdated (36). These six base64url digits fit in a JS number.
  let timestamp = 0;
  for (const digit of value.slice(7, 13)) {
    timestamp = timestamp * 64 + BASE64URL.indexOf(digit);
  }
  return timestamp;
}

function mirrorDecision(doc: Document, data: TcfData): void {
  const existing = readCookie(doc, COOKIE);
  const attributes = 'Path=/; Secure; SameSite=Lax';
  const clear = () => {
    if (existing !== undefined) doc.cookie = `${COOKIE}=; ${attributes}; Max-Age=0`;
  };

  if (readCookie(doc, CMP_COOKIE) !== undefined || data.gdprApplies === false) {
    clear();
    return;
  }
  if (data.gdprApplies !== true) return;

  const value = data.tcString;
  if (typeof value !== 'string') {
    clear();
    return;
  }
  const updated = lastUpdated(value);
  if (updated === undefined) {
    clear();
    return;
  }

  if (existing === value) return;
  const previous = existing === undefined ? undefined : lastUpdated(existing);
  // A loaded snapshot from another tab must not undo a newer decision. At
  // the same timestamp, accept a user action (which can change other purposes
  // or vendors) or an explicit storage denial.
  if (
    previous !== undefined &&
    (previous > updated ||
      (previous === updated &&
        data.eventStatus !== 'useractioncomplete' &&
        data.purpose?.consents?.[1] !== false))
  ) {
    return;
  }

  // Keep the full CMP string, including denials, for server-side decoding.
  doc.cookie = `${COOKIE}=${value}; ${attributes}`;
}

/** Mirrors settled Sourcepoint TCF decisions for subsequent first-party requests. */
export function installSourcepointTcfMirror(target: TcfWindow): () => void {
  if (target.location.protocol !== 'https:' || target.top !== target) return () => {};

  const doc = target.document;
  let active: TcfSubscription | undefined;
  let subscribing = false;

  function getApi(): TcfApi | undefined {
    try {
      return target.__tcfapi;
    } catch {
      return undefined;
    }
  }

  function removeListener(subscription: TcfSubscription): void {
    const id = subscription.listenerId;
    if (id === undefined) return;
    subscription.listenerId = undefined;
    try {
      subscription.api.call(target, 'removeEventListener', 2, () => {}, id);
    } catch {
      log.debug('sourcepoint: TCF listener cleanup was unavailable');
    }
  }

  function unsubscribe(): void {
    const current = active;
    active = undefined;
    if (current) removeListener(current);
  }

  function subscribe(): void {
    if (subscribing) return;
    const api = getApi();
    if (active?.api === api) return;
    unsubscribe();
    if (typeof api !== 'function') return;

    const subscription: TcfSubscription = { api };
    active = subscription;
    subscribing = true;
    try {
      api.call(target, 'addEventListener', 2, (data, success) => {
        // A queued stub may report its listener ID only after it was retired.
        if (typeof data?.listenerId === 'number') subscription.listenerId = data.listenerId;
        if (active !== subscription) {
          removeListener(subscription);
          return;
        }
        if (getApi() !== api) {
          // A stub can hand its queued callback to the real API after page load.
          subscribe();
          return;
        }
        if (!success) {
          unsubscribe();
          return;
        }
        if (!data) return;
        if (
          data.cmpStatus !== 'loaded' ||
          (data.eventStatus !== 'tcloaded' && data.eventStatus !== 'useractioncomplete')
        ) {
          return;
        }
        try {
          mirrorDecision(doc, data);
        } catch {
          log.debug('sourcepoint: TCF cookie storage is unavailable');
        }
      });
    } catch {
      unsubscribe();
      log.debug('sourcepoint: TCF listener registration was unavailable');
    } finally {
      subscribing = false;
    }
  }

  subscribe();
  // The Sourcepoint stub or SDK may load after the Trusted Server bundle.
  doc.addEventListener('DOMContentLoaded', subscribe);
  doc.addEventListener('load', subscribe, true);
  target.addEventListener('pageshow', subscribe);
  target.addEventListener('focus', subscribe);
  return () => {
    doc.removeEventListener('DOMContentLoaded', subscribe);
    doc.removeEventListener('load', subscribe, true);
    target.removeEventListener('pageshow', subscribe);
    target.removeEventListener('focus', subscribe);
    unsubscribe();
  };
}
