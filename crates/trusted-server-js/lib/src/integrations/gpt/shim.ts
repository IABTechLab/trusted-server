import { log } from '../../core/log';

import type { GoogleTag, GptWindow } from './types';
import { installGptGuard } from './script_guard';

const executingPublisherScript = typeof document === 'undefined' ? null : document.currentScript;
let attributionInstalled = false;

// ------------------------------------------------------------------
// Shim implementation
// ------------------------------------------------------------------

/**
 * Ensure the `googletag` stub exists on `window`.
 *
 * This mirrors the official GPT bootstrap snippet:
 * ```js
 * window.googletag = window.googletag || {};
 * googletag.cmd = googletag.cmd || [];
 * ```
 * By running before the publisher's own snippet we can patch `cmd` early.
 */
function ensureGoogleTagStub(win: GptWindow): Partial<GoogleTag> {
  const tag = (win.googletag = win.googletag ?? {});
  tag.cmd = tag.cmd ?? [];
  return tag;
}

export function installTrustedServerPageTargeting(): void {
  const owner = (window as GptWindow).__tsjs_gpt_early;
  if (owner && owner.installPageTargeting !== installTrustedServerPageTargeting) {
    owner.installPageTargeting();
    return;
  }
  if (
    attributionInstalled ||
    executingPublisherScript?.getAttribute('data-ts-gam-attribution') !== 'true'
  ) {
    return;
  }

  const win = window as GptWindow;
  const tag = ensureGoogleTagStub(win);
  attributionInstalled = true;
  tag.cmd!.push(() => {
    try {
      const gpt = win.googletag;
      if (typeof gpt?.setConfig === 'function') {
        gpt.setConfig({ targeting: { ts: 'true' } });
      }
    } catch (error) {
      log.warn('[tsjs-gpt] GAM attribution targeting failed', error);
    }
  });
}

/**
 * Wrap a queued GPT callback to add instrumentation and future hook points.
 *
 * Today the wrapper only logs; as the integration matures it will inject
 * EC ID targeting and consent gates.
 */
function wrapCommand(fn: () => void): () => void {
  return () => {
    try {
      fn();
    } catch (err) {
      log.error('GPT shim: queued command threw', err);
    }
  };
}

/**
 * Patch `googletag.cmd` so every pushed callback runs through [`wrapCommand`].
 *
 * Preserves the existing `tag.cmd` array identity so that GPT's own custom
 * `cmd.push` behaviour (immediate execution when the library is already
 * loaded) is not lost. The original `push` is saved and delegated to after
 * wrapping each callback.
 *
 * Already-queued callbacks are re-wrapped in place so GPT processes them
 * through our wrapper when it drains the queue.
 */
function patchCommandQueue(tag: Partial<GoogleTag>): void {
  // Ensure the queue exists.
  if (!tag.cmd) {
    // Cast through unknown so an array satisfies the { push } type.
    tag.cmd = [];
  }

  const queue = tag.cmd;

  // Guard against double-patching (idempotent install).
  if ((queue as { __tsPushed?: boolean }).__tsPushed) {
    log.debug('GPT shim: command queue already patched, skipping');
    return;
  }

  const originalPush = queue.push.bind(queue);

  // Override push on the *existing* array — preserves object identity so
  // GPT (if already loaded) keeps its reference.
  (queue as { push: (...cbs: Array<() => void>) => unknown }).push = function (
    ...callbacks: Array<() => void>
  ): unknown {
    const wrapped = callbacks.map(wrapCommand);
    return originalPush(...wrapped);
  };

  // Mark as patched to prevent double-wrapping.
  (queue as { __tsPushed?: boolean }).__tsPushed = true;

  // Re-wrap any callbacks that were queued before we patched.
  // Only applicable when cmd is an array (pre-GPT-load case).
  if (Array.isArray(queue)) {
    for (let i = 0; i < queue.length; i++) {
      queue[i] = wrapCommand(queue[i]);
    }
    log.debug('GPT shim: command queue patched', { pendingCommands: queue.length });
  } else {
    log.debug('GPT shim: command queue patched');
  }
}

/**
 * Install the GPT integration shim.
 *
 * Sets up the script guard for dynamic script interception and patches the
 * `googletag.cmd` command queue.
 */
export function installGptShim(): boolean {
  if (typeof window === 'undefined') {
    return false;
  }

  const win = window as GptWindow;
  // Reuse the early IIFE's native hooks instead of wrapping them a second time.
  const owner = win.__tsjs_gpt_early;
  if (owner && owner.installShim !== installGptShim) return owner.installShim();

  // Install DOM interception guard first so any dynamic GPT script insertions
  // are rewritten before the browser fetches them.
  installGptGuard();

  const tag = ensureGoogleTagStub(win);
  patchCommandQueue(tag);

  log.info('GPT shim installed');
  return true;
}
