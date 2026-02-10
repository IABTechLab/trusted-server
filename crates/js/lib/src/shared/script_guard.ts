import { log } from '../core/log';

/**
 * Shared Script Guard Factory
 *
 * Creates a DOM interception guard that patches appendChild and insertBefore
 * to intercept dynamically inserted script (and preload/prefetch link) elements
 * whose URLs match an integration's SDK. The matched URLs are rewritten to a
 * first-party proxy endpoint before the element is inserted into the DOM.
 *
 * Each call to createScriptGuard() produces an independent guard with its own
 * installation state, so multiple integrations can coexist without interference.
 */

/**
 * Base configuration shared by all guard types.
 */
interface ScriptGuardConfigBase {
  /** Integration name used in log messages (e.g. "Lockr", "Permutive"). */
  name: string;
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
  /** Patch appendChild/insertBefore to intercept matching scripts. */
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
 * Determine whether a DOM node is a script or preload/prefetch link element
 * whose URL matches the guard's target pattern.
 */
function shouldRewriteElement(
  node: Node,
  isTargetUrl: (url: string) => boolean
): node is HTMLScriptElement | HTMLLinkElement {
  if (!(node instanceof HTMLElement)) {
    return false;
  }

  // Script elements
  if (node.tagName === 'SCRIPT') {
    const src = (node as HTMLScriptElement).src || node.getAttribute('src');
    return !!src && isTargetUrl(src);
  }

  // Link preload/prefetch elements
  if (node.tagName === 'LINK') {
    const link = node as HTMLLinkElement;
    const rel = link.getAttribute('rel');
    if ((rel !== 'preload' && rel !== 'prefetch') || link.getAttribute('as') !== 'script') {
      return false;
    }
    const href = link.href || link.getAttribute('href');
    return !!href && isTargetUrl(href);
  }

  return false;
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
function rewriteElement(
  element: HTMLScriptElement | HTMLLinkElement,
  config: ScriptGuardConfig
): void {
  const prefix = `${config.name} guard`;

  if (element.tagName === 'SCRIPT') {
    const script = element as HTMLScriptElement;
    const originalSrc = script.src || script.getAttribute('src');
    if (!originalSrc) return;

    const rewritten = getRewrittenUrl(originalSrc, config);

    log.info(`${prefix}: rewriting dynamically inserted SDK script`, {
      original: originalSrc,
      rewritten,
      framework: script.getAttribute('data-nscript') || 'generic',
    });

    script.src = rewritten;
    script.setAttribute('src', rewritten);
  } else if (element.tagName === 'LINK') {
    const link = element as HTMLLinkElement;
    const originalHref = link.href || link.getAttribute('href');
    if (!originalHref) return;

    const rewritten = getRewrittenUrl(originalHref, config);

    log.info(`${prefix}: rewriting SDK ${link.getAttribute('rel')} link`, {
      original: originalHref,
      rewritten,
      rel: link.getAttribute('rel'),
      as: link.getAttribute('as'),
    });

    link.href = rewritten;
    link.setAttribute('href', rewritten);
  }
}

/**
 * Create an independent script guard for a specific integration.
 */
export function createScriptGuard(config: ScriptGuardConfig): ScriptGuard {
  let installed = false;
  const prefix = `${config.name} guard`;

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

    const originalAppendChild = Element.prototype.appendChild;
    const originalInsertBefore = Element.prototype.insertBefore;

    Element.prototype.appendChild = function <T extends Node>(this: Element, node: T): T {
      if (shouldRewriteElement(node, config.isTargetUrl)) {
        rewriteElement(node as HTMLScriptElement | HTMLLinkElement, config);
      }
      return originalAppendChild.call(this, node) as T;
    };

    Element.prototype.insertBefore = function <T extends Node>(
      this: Element,
      node: T,
      reference: Node | null
    ): T {
      if (shouldRewriteElement(node, config.isTargetUrl)) {
        rewriteElement(node as HTMLScriptElement | HTMLLinkElement, config);
      }
      return originalInsertBefore.call(this, node, reference) as T;
    };

    installed = true;
    log.info(`${prefix}: DOM interception installed successfully`);
  }

  function isInstalled(): boolean {
    return installed;
  }

  function reset(): void {
    installed = false;
  }

  return { install, isInstalled, reset };
}
