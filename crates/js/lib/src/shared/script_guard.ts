import { log } from '../core/log';

/**
 * Shared Script Guard Factory
 *
 * Creates a DOM interception guard that patches appendChild and insertBefore
 * to intercept dynamically inserted script (and preload link) elements whose
 * URLs match an integration's SDK. The matched URLs are rewritten to a
 * first-party proxy endpoint before the element is inserted into the DOM.
 *
 * Each call to createScriptGuard() produces an independent guard with its own
 * installation state, so multiple integrations can coexist without interference.
 */

export interface ScriptGuardConfig {
  /** Integration name used in log messages (e.g. "Lockr", "Permutive"). */
  name: string;
  /** Return true if the URL belongs to this integration's SDK. */
  isTargetUrl: (url: string) => boolean;
  /** First-party proxy path to rewrite to (e.g. "/integrations/lockr/sdk"). */
  proxyPath: string;
}

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
  const protocol = window.location.protocol === 'https:' ? 'https' : 'http';
  const host = window.location.host;
  return `${protocol}://${host}${proxyPath}`;
}

/**
 * Determine whether a DOM node is a script or preload-link element whose URL
 * matches the guard's target pattern.
 */
function shouldRewriteElement(
  node: Node,
  isTargetUrl: (url: string) => boolean,
): node is HTMLScriptElement | HTMLLinkElement {
  if (!node || !(node instanceof HTMLElement)) {
    return false;
  }

  // Script elements
  if (node.tagName === 'SCRIPT') {
    const src =
      (node as HTMLScriptElement).src || node.getAttribute('src');
    return !!src && isTargetUrl(src);
  }

  // Link preload elements
  if (node.tagName === 'LINK') {
    const link = node as HTMLLinkElement;
    if (link.getAttribute('rel') !== 'preload' || link.getAttribute('as') !== 'script') {
      return false;
    }
    const href = link.href || link.getAttribute('href');
    return !!href && isTargetUrl(href);
  }

  return false;
}

/**
 * Rewrite the URL attribute on a matched element to the first-party proxy.
 */
function rewriteElement(
  element: HTMLScriptElement | HTMLLinkElement,
  config: ScriptGuardConfig,
): void {
  const prefix = `${config.name} guard`;

  if (element.tagName === 'SCRIPT') {
    const script = element as HTMLScriptElement;
    const originalSrc = script.src || script.getAttribute('src');
    if (!originalSrc) return;

    const rewritten = rewriteToFirstParty(config.proxyPath);

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

    const rewritten = rewriteToFirstParty(config.proxyPath);

    log.info(`${prefix}: rewriting SDK preload link`, {
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
      reference: Node | null,
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
