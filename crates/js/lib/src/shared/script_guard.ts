import { log } from '../core/log';

import {
  DEFAULT_DOM_INSERTION_HANDLER_PRIORITY,
  registerDomInsertionHandler,
  type DomInsertionCandidate,
} from './dom_insertion_dispatcher';

/**
 * Shared Script Guard Factory
 *
 * Creates a DOM interception guard that registers with the shared DOM insertion
 * dispatcher. Matching dynamically inserted script (and preload/prefetch link)
 * elements are rewritten to a first-party proxy endpoint before insertion.
 *
 * Each call to createScriptGuard() produces an independent guard with its own
 * installation state, so multiple integrations can coexist without interference.
 */

/**
 * Base configuration shared by all guard types.
 */
interface ScriptGuardConfigBase {
  /** Integration ID used for deterministic ordering and internal identity. */
  id: string;
  /** Optional human-readable label used in log messages (e.g. "GTM"). */
  displayName?: string;
  /** Lower values run earlier when multiple handlers match the same node. */
  priority?: number;
  /** Return true if the URL belongs to this integration's SDK. */
  isTargetUrl: (url: string) => boolean;
}

/**
 * Config using a fixed proxy path (original behavior).
 * The entire URL is replaced with `{origin}{proxyPath}`.
 */
interface ScriptGuardConfigWithProxyPath extends ScriptGuardConfigBase {
  /** First-party proxy path to rewrite to (e.g. "/integrations/lockr/sdk"). */
  proxyPath: string;
  rewriteUrl?: never;
}

/**
 * Config using a custom URL rewriter function.
 * Allows integrations like DataDome to preserve the original path.
 */
interface ScriptGuardConfigWithRewriter extends ScriptGuardConfigBase {
  proxyPath?: never;
  /** Custom function to rewrite the original URL to a first-party URL. */
  rewriteUrl: (originalUrl: string) => string;
}

export type ScriptGuardConfig = ScriptGuardConfigWithProxyPath | ScriptGuardConfigWithRewriter;

export interface ScriptGuard {
  /** Install a shared DOM insertion handler for matching scripts and links. */
  install: () => void;
  /** Whether the guard has already been installed. */
  isInstalled: () => boolean;
  /** Reset installation state (primarily for testing). */
  reset: () => void;
}

/**
 * Build a first-party URL from the current page origin and the configured proxy path.
 */
function rewriteToFirstParty(proxyPath: string): string {
  return `${window.location.origin}${proxyPath}`;
}

/**
 * Get the rewritten URL using either the custom rewriter or the proxy path.
 */
function getRewrittenUrl(originalUrl: string, config: ScriptGuardConfig): string {
  if (config.rewriteUrl) {
    return config.rewriteUrl(originalUrl);
  }
  return rewriteToFirstParty(config.proxyPath);
}

/**
 * Rewrite the URL attribute on a matched element to the first-party proxy.
 */
function rewriteElement(candidate: DomInsertionCandidate, config: ScriptGuardConfig): void {
  const prefix = `${config.displayName ?? config.id} guard`;

  if (candidate.kind === 'script') {
    const rewritten = getRewrittenUrl(candidate.url, config);

    log.info(`${prefix}: rewriting dynamically inserted SDK script`, {
      original: candidate.url,
      rewritten,
      framework: candidate.element.getAttribute('data-nscript') || 'generic',
    });

    candidate.element.src = rewritten;
    candidate.element.setAttribute('src', rewritten);
  } else {
    const rewritten = getRewrittenUrl(candidate.url, config);

    log.info(`${prefix}: rewriting SDK ${candidate.rel} link`, {
      original: candidate.url,
      rewritten,
      rel: candidate.rel,
      as: candidate.element.getAttribute('as'),
    });

    candidate.element.href = rewritten;
    candidate.element.setAttribute('href', rewritten);
  }
}

/**
 * Create an independent script guard for a specific integration.
 */
export function createScriptGuard(config: ScriptGuardConfig): ScriptGuard {
  let installed = false;
  let unregister: (() => void) | undefined;
  const prefix = `${config.displayName ?? config.id} guard`;

  function install(): void {
    if (installed) {
      log.debug(`${prefix}: already installed, skipping`);
      return;
    }

    if (typeof window === 'undefined' || typeof Element === 'undefined') {
      log.debug(`${prefix}: not in browser environment, skipping`);
      return;
    }

    log.info(`${prefix}: installing DOM interception for SDK`);

    unregister = registerDomInsertionHandler({
      handle(candidate): boolean {
        if (!config.isTargetUrl(candidate.url)) {
          return false;
        }

        rewriteElement(candidate, config);
        return true;
      },
      id: config.id,
      priority: config.priority ?? DEFAULT_DOM_INSERTION_HANDLER_PRIORITY,
    });

    installed = true;
    log.info(`${prefix}: DOM interception installed successfully`);
  }

  function isInstalled(): boolean {
    return installed;
  }

  function reset(): void {
    if (unregister) {
      unregister();
      unregister = undefined;
    }

    if (installed) {
      log.debug(`${prefix}: reset and uninstalled`);
    }

    installed = false;
  }

  return { install, isInstalled, reset };
}
