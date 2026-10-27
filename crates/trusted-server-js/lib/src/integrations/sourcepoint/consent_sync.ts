import { log } from '../../core/log';

export interface ConsentSnapshot {
  gppString: string | null;
  applicableSections: number[];
}

/** Publishes each real decision without coalescing away an intervening withdrawal. */
export class ConsentSync {
  private lastSnapshot?: string;
  private stopped = false;

  constructor(private readonly fetch: typeof window.fetch) {}

  publish(snapshot: ConsentSnapshot): void {
    if (this.stopped) return;
    const body = JSON.stringify(snapshot);
    if (body === this.lastSnapshot) return;
    this.lastSnapshot = body;
    // Each request carries its own snapshot. Cookie reads at network dispatch
    // must not replace an earlier opt-out with a later decision. The endpoint
    // cannot mint or restore identity, so response order cannot revive an EC.
    try {
      void this.fetch('/_ts/consent', {
        method: 'POST',
        credentials: 'same-origin',
        redirect: 'error',
        cache: 'no-store',
        keepalive: true,
        headers: { 'Content-Type': 'application/json' },
        body,
      })
        .then((response) => {
          // The endpoint returns no identity data; discard any error body too.
          void response.body?.cancel().catch(() => {});
          if (response.status !== 204) this.failed(body);
        })
        .catch(() => this.failed(body));
    } catch {
      this.failed(body);
    }
  }

  stop(): void {
    // An in-flight withdrawal must survive navigation; do not abort keepalive.
    this.stopped = true;
  }

  private failed(body: string): void {
    // Retry on a subsequent CMP/focus event, not a background retry loop.
    if (this.lastSnapshot === body) this.lastSnapshot = undefined;
    log.warn('sourcepoint: server consent reconciliation was not confirmed');
  }
}
