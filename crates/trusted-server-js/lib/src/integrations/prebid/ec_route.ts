import { log } from '../../core/log';

import {
  ConsentQueue,
  isKnownWithdrawal,
  readConsentCookies,
  requestBody,
  type ConsentSnapshot,
  type CookieView,
  type QueueEntry,
} from './consent_queue';
import { GppConsentCapture, type GppWindow } from './gpp_capture';
import {
  expireMirrorCookie,
  liveMirrorEids,
  parseMirrorEnvelope,
  readMirror,
  readMirrorCookieValue,
  type MirrorEid,
} from './identity';
import { TcfReader } from './tcf_reader';

const MAX_RESPONSE_BYTES = 16 * 1024;

type Fetch = (input: string, init: RequestInit) => Promise<Response>;

/** Returns the injected route path when it is a path on this page's origin. */
export function validEcRoute(value: unknown, location: Location): string | undefined {
  if (typeof value !== 'string' || !value.startsWith('/') || value.startsWith('//')) {
    return undefined;
  }
  try {
    return new URL(value, location.origin).origin === location.origin ? value : undefined;
  } catch {
    return undefined;
  }
}

async function readResult(response: Response): Promise<MirrorEid[] | undefined> {
  if (response.status === 204) return [];
  if (response.status !== 200) return undefined;
  const declared = Number(response.headers.get('Content-Length'));
  if (declared > MAX_RESPONSE_BYTES) return undefined;
  const text = await response.text();
  if (new TextEncoder().encode(text).length > MAX_RESPONSE_BYTES) return undefined;
  return parseMirrorEnvelope(JSON.parse(text));
}

function consentCookies(): CookieView {
  try {
    return readConsentCookies(document.cookie);
  } catch {
    // Unreadable cookies cannot show what the navigation carried.
    return { euconsent: null, gpp: null };
  }
}

/**
 * Reports CMP consent to the EC route in order and decides which mirrored IDs
 * identity mode may merge into a publisher auction.
 */
export class EcRouteClient {
  private readonly queue = new ConsentQueue();
  private sending = false;
  private suppressed = false;
  // A known withdrawal that cannot be sent queues nothing, so the grant
  // already in flight would otherwise lift its suppression when answered. An
  // answer lifts suppression only when its call was queued after the latest
  // such fence.
  private fenceEpoch = 0;
  private readonly entryEpochs = new WeakMap<QueueEntry, number>();
  private applied?: { eids: MirrorEid[]; cookie: string | undefined };

  constructor(
    private readonly path: string,
    private readonly fetch: Fetch,
    private readonly beforeSend: () => void
  ) {}

  /** Records a new CMP snapshot and sends it when its turn comes. */
  report(snapshot: ConsentSnapshot): void {
    if (isKnownWithdrawal(snapshot)) {
      // A grant in flight may still deliver the mirror before this is answered.
      this.suppress();
      expireMirrorCookie();
      if (requestBody(snapshot) === undefined) this.fenceEpoch++;
    }
    const entry = this.queue.offer(snapshot, consentCookies());
    if (entry?.kind === 'barrier') this.suppress();
    if (entry) this.entryEpochs.set(entry, this.fenceEpoch);
    this.pump();
  }

  private suppress(): void {
    this.applied = undefined;
    this.suppressed = true;
  }

  /** Sends the head of the queue unless a call is in flight. */
  pump(): void {
    if (this.sending) return;
    this.beforeSend();
    const entry = this.queue.next();
    if (!entry) return;
    this.sending = true;
    let request: Promise<Response>;
    try {
      request = this.fetch(this.path, {
        method: 'POST',
        credentials: 'same-origin',
        cache: 'no-store',
        redirect: 'error',
        keepalive: true,
        headers: { 'Content-Type': 'application/json' },
        body: entry.body,
      });
    } catch (error) {
      request = Promise.reject(error);
    }
    void request
      .then(readResult)
      .catch(() => undefined)
      .then((eids) => {
        this.sending = false;
        if (eids === undefined) {
          log.warn('[tsjs-prebid] EC route consent report was not confirmed');
          // A failed barrier waits for a trigger rather than retrying in a
          // loop; a dropped grant must not hold back what queued behind it.
          if (this.queue.failed() === 'grant') this.pump();
          return;
        }
        if (this.queue.succeeded()) {
          if (this.entryEpochs.get(entry) === this.fenceEpoch) {
            this.applied = { eids, cookie: readMirrorCookieValue() };
            this.suppressed = false;
          }
        } else {
          // Overtaken by a waiting change: never fall back to an older result.
          this.applied = undefined;
        }
        this.pump();
      });
  }

  /** The IDs an identity-mode auction may merge right now. */
  mirrorSource(): MirrorEid[] {
    if (this.suppressed) return [];
    if (this.applied && readMirrorCookieValue() === this.applied.cookie) {
      return liveMirrorEids(this.applied.eids);
    }
    return readMirror();
  }
}

/**
 * Starts consent capture for the EC route. Readings from the publisher's TCF
 * and GPP CMPs feed the client; page lifecycle events rediscover a late or
 * replaced CMP and resend a failed withdrawal.
 */
export function installEcRouteClient(
  path: string,
  beforeSend: () => void
): {
  client: EcRouteClient;
  discover: () => void;
} {
  const target = window as GppWindow & { __tcfapi?: unknown };
  const client = new EcRouteClient(path, window.fetch.bind(window), beforeSend);
  let latest: ConsentSnapshot = {};
  const tcf = new TcfReader(target, (reading) => {
    latest = { ...latest, tcf: reading };
    client.report(latest);
  });
  const gpp = new GppConsentCapture(
    target,
    (consent) => {
      latest = { ...latest, gpp: consent };
      client.report(latest);
    },
    // Not-ready, not-applicable and failed reads are not decisions; keep the
    // previous reading rather than reporting a change.
    () => {}
  );
  let gppApi = target.__gpp;
  const discover = (): void => {
    tcf.refresh();
    if (target.__gpp !== gppApi) {
      gppApi = target.__gpp;
      gpp.refresh();
    }
  };
  const onLifecycle = (): void => {
    discover();
    client.pump();
  };
  gpp.start();
  tcf.refresh();
  document.addEventListener('DOMContentLoaded', onLifecycle);
  document.addEventListener('load', discover, true);
  document.addEventListener('visibilitychange', onLifecycle);
  window.addEventListener('pageshow', (event) => {
    if (event.persisted) onLifecycle();
  });
  window.addEventListener('focus', () => client.pump());
  return { client, discover };
}
