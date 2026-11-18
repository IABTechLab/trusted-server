import { isCookieSafeGppString } from './gpp_capture';
import type { TcfReading } from './tcf_reader';

/** One ready GPP decision. */
export interface GppReading {
  gppString: string;
  applicableSections: number[];
}

/** The latest reading of each CMP API present on the page. */
export interface ConsentSnapshot {
  tcf?: TcfReading;
  gpp?: GppReading;
}

/**
 * Consent cookies the navigation request could have carried: `null` when the
 * name appears more than once, since the browser and server may pick
 * different copies.
 */
export interface CookieView {
  euconsent?: string | null;
  gpp?: string | null;
}

export type ConsentKind = 'grant' | 'barrier';

export interface QueueEntry {
  snapshot: ConsentSnapshot;
  body: string;
  kind: ConsentKind;
}

const TC_STRING = /^[A-Za-z0-9._-]+$/;

function sendableTcf(reading: TcfReading | undefined): boolean {
  return !!reading && (!reading.gdprApplies || TC_STRING.test(reading.tcString));
}

/** Reads one consent cookie, reporting a duplicated name as `null`. */
export function readConsentCookies(cookieHeader: string): CookieView {
  const view: Record<string, string | null> = {};
  for (const part of cookieHeader.split(';')) {
    const separator = part.indexOf('=');
    const name = part.slice(0, separator).trim();
    const key = name === 'euconsent-v2' ? 'euconsent' : name === '__gpp' ? 'gpp' : undefined;
    if (separator < 0 || !key) continue;
    view[key] = key in view ? null : part.slice(separator + 1).trim();
  }
  return view;
}

function sendableGpp(reading: GppReading | undefined): reading is GppReading {
  return (
    !!reading &&
    isCookieSafeGppString(reading.gppString) &&
    reading.applicableSections.every((id) => Number.isInteger(id) && id > 0 && id <= 65535)
  );
}

/**
 * Removes readings the route cannot receive. A GDPR reading with an invalid
 * TC string is dropped rather than sent without its string.
 */
export function sendableSnapshot(snapshot: ConsentSnapshot): ConsentSnapshot {
  return {
    ...(sendableTcf(snapshot.tcf) ? { tcf: snapshot.tcf } : {}),
    ...(sendableGpp(snapshot.gpp) ? { gpp: snapshot.gpp } : {}),
  };
}

/** The JSON body for the EC route, or `undefined` when nothing is sendable. */
export function requestBody(snapshot: ConsentSnapshot): string | undefined {
  const { tcf, gpp } = sendableSnapshot(snapshot);
  if (!tcf?.gdprApplies && !gpp) return undefined;
  const body: Record<string, unknown> = {};
  // Out-of-scope TCF sends nothing: the server keys GDPR off the TC string, so
  // asking it to drop the cookie would release IDs a navigation withholds.
  if (tcf?.gdprApplies) body.tcString = tcf.tcString;
  if (gpp) {
    body.gppString = gpp.gppString;
    body.applicableSections = gpp.applicableSections;
  }
  return JSON.stringify(body);
}

/** TCF says GDPR applies and Purpose 1 or 4 is not consented. */
export function isKnownWithdrawal(snapshot: ConsentSnapshot): boolean {
  const tcf = snapshot.tcf;
  return !!tcf?.gdprApplies && !(tcf.granted.includes('p1') && tcf.granted.includes('p4'));
}

function tcfBarrier(
  current: TcfReading | undefined,
  previous: TcfReading | undefined,
  cookies: CookieView
): boolean {
  if (!current) return false;
  if (!previous) return current.gdprApplies && current.tcString !== cookies.euconsent;
  if (current.gdprApplies && !previous.gdprApplies) return true;
  const now = new Set(current.granted);
  return previous.granted.some((grant) => !now.has(grant));
}

function gppBarrier(
  current: GppReading | undefined,
  previous: GppReading | undefined,
  cookies: CookieView
): boolean {
  if (!current) return false;
  if (!previous) return current.gppString !== cookies.gpp;
  return (
    current.gppString !== previous.gppString ||
    current.applicableSections.join(',') !== previous.applicableSections.join(',')
  );
}

/**
 * Classifies a snapshot against the one before it. A barrier may withdraw
 * something, so it is never replaced while it waits.
 */
export function classify(
  snapshot: ConsentSnapshot,
  previous: ConsentSnapshot | undefined,
  cookies: CookieView
): ConsentKind {
  return isKnownWithdrawal(snapshot) ||
    tcfBarrier(snapshot.tcf, previous?.tcf, cookies) ||
    gppBarrier(snapshot.gpp, previous?.gpp, cookies)
    ? 'barrier'
    : 'grant';
}

/**
 * Orders consent reports to the EC route: one call in flight, newer grants
 * replace a waiting grant, and waiting barriers hold everything behind them.
 */
export class ConsentQueue {
  private waiting: QueueEntry[] = [];
  private inFlight?: QueueEntry;
  private lastSent?: QueueEntry;

  private tail(): QueueEntry | undefined {
    return this.waiting[this.waiting.length - 1] ?? this.inFlight ?? this.lastSent;
  }

  /** Queues a snapshot; returns the new entry, or `undefined` when it is ignored. */
  offer(raw: ConsentSnapshot, cookies: CookieView): QueueEntry | undefined {
    const snapshot = sendableSnapshot(raw);
    const body = requestBody(snapshot);
    const tail = this.tail();
    if (body === undefined || body === tail?.body) return undefined;
    const entry: QueueEntry = { snapshot, body, kind: classify(snapshot, tail?.snapshot, cookies) };
    const last = this.waiting[this.waiting.length - 1];
    if (last?.kind === 'grant') this.waiting[this.waiting.length - 1] = entry;
    else this.waiting.push(entry);
    return entry;
  }

  /**
   * Stops treating the last answered call as the server's current state, so a
   * reading equal to it is sent again. Waiting and in-flight calls still
   * deduplicate.
   */
  forgetSent(): void {
    this.lastSent = undefined;
  }

  /** Moves the head into flight, unless a call is already in flight. */
  next(): QueueEntry | undefined {
    if (this.inFlight || this.waiting.length === 0) return undefined;
    this.inFlight = this.waiting.shift();
    return this.inFlight;
  }

  /** Records success; returns whether its result may be applied (nothing waits). */
  succeeded(): boolean {
    this.lastSent = this.inFlight;
    this.inFlight = undefined;
    return this.waiting.length === 0;
  }

  /**
   * A failed barrier returns to the head; a failed grant is dropped. Returns
   * the failed entry's kind.
   */
  failed(): ConsentKind | undefined {
    const entry = this.inFlight;
    this.inFlight = undefined;
    if (entry?.kind === 'barrier') this.waiting.unshift(entry);
    return entry?.kind;
  }
}
