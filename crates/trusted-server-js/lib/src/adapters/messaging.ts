export type CaptureMessageListener = (event: MessageEvent) => void;

/** Exact browser event surface owned by the cross-window messaging adapter. */
export interface MessageEventTarget {
  addEventListener(type: 'message', listener: CaptureMessageListener, capture: true): void;
  removeEventListener(type: 'message', listener: CaptureMessageListener, capture: true): void;
}

/** Cross-window boundary consumed by the kernel's capability recognizer. */
export interface MessagingAdapter {
  installCaptureListener(listener: CaptureMessageListener): () => void;
}

/**
 * Create the production messaging boundary.
 *
 * Listener installation is deliberately synchronous so core can reserve a
 * capability message before any integration activation or TS-owned injection.
 */
export function createBrowserMessagingAdapter(
  target: MessageEventTarget = window as unknown as MessageEventTarget
): MessagingAdapter {
  return Object.freeze({
    installCaptureListener(listener: CaptureMessageListener): () => void {
      target.addEventListener('message', listener, true);
      let installed = true;

      return () => {
        if (!installed) return;
        installed = false;
        target.removeEventListener('message', listener, true);
      };
    },
  });
}

/** Create a side-effect-free messaging boundary for tests and non-DOM runtimes. */
export function createNoopMessagingAdapter(): MessagingAdapter {
  return Object.freeze({
    installCaptureListener: () => () => undefined,
  });
}
