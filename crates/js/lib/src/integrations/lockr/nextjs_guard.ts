import { log } from '../../core/log';

/**
 * Next.js Guard for Lockr SDK Script Interception
 *
 * This module intercepts Next.js dynamic script loading to rewrite Lockr SDK URLs
 * to first-party endpoints. Next.js loads scripts using appendChild/insertBefore
 * with data-nscript="afterInteractive", which bypasses server-side HTML rewriting.
 *
 * The guard patches DOM methods to catch these dynamic insertions and rewrite
 * Lockr SDK URLs to use the first-party domain proxy endpoint.
 */

let guardInstalled = false;

/**
 * Check if a URL is a Lockr SDK URL.
 * Matches the logic from lockr.rs:79-86
 */
function isLockrSdkUrl(url: string): boolean {
  if (!url) return false;

  const lower = url.toLowerCase();

  // Check for aim.loc.kr domain
  if (lower.includes('aim.loc.kr')) {
    return true;
  }

  // Check for identity.loc.kr with identity-lockr and .js extension
  if (
    lower.includes('identity.loc.kr') &&
    lower.includes('identity-lockr') &&
    lower.endsWith('.js')
  ) {
    return true;
  }

  return false;
}

/**
 * Rewrite a Lockr SDK URL to use the first-party domain proxy endpoint.
 */
function rewriteToFirstParty(originalUrl: string): string {
  const protocol = window.location.protocol === 'https:' ? 'https' : 'http';
  const host = window.location.host;
  return `${protocol}://${host}/integrations/lockr/sdk`;
}

/**
 * Check if an element should be rewritten.
 * Returns true for:
 * - Script elements with data-nscript="afterInteractive" and Lockr SDK URL
 * - Link elements with rel="preload" as="script" and Lockr SDK URL
 */
function shouldRewriteElement(element: Node): element is HTMLScriptElement | HTMLLinkElement {
  if (!element || !(element instanceof HTMLElement)) {
    return false;
  }

  // Handle script elements
  if (element.tagName === 'SCRIPT') {
    const scriptElement = element as HTMLScriptElement;

    // Check for Next.js data-nscript attribute
    const nscript = scriptElement.getAttribute('data-nscript');
    if (nscript !== 'afterInteractive') {
      return false;
    }

    // Check if src is a Lockr SDK URL
    const src = scriptElement.src || scriptElement.getAttribute('src');
    if (!src) {
      return false;
    }

    return isLockrSdkUrl(src);
  }

  // Handle link preload elements
  if (element.tagName === 'LINK') {
    const linkElement = element as HTMLLinkElement;

    // Check if it's a preload link for a script
    const rel = linkElement.getAttribute('rel');
    const as = linkElement.getAttribute('as');
    if (rel !== 'preload' || as !== 'script') {
      return false;
    }

    // Check if href is a Lockr SDK URL
    const href = linkElement.href || linkElement.getAttribute('href');
    if (!href) {
      return false;
    }

    return isLockrSdkUrl(href);
  }

  return false;
}

/**
 * Rewrite an element's URL attribute to use first-party proxy.
 * Handles both script src and link href attributes.
 */
function rewriteElement(element: HTMLScriptElement | HTMLLinkElement): void {
  if (element.tagName === 'SCRIPT') {
    const scriptElement = element as HTMLScriptElement;
    const originalSrc = scriptElement.src || scriptElement.getAttribute('src');
    if (!originalSrc) return;

    const rewrittenSrc = rewriteToFirstParty(originalSrc);

    log.info('Next.js guard: rewriting Lockr SDK script', {
      original: originalSrc,
      rewritten: rewrittenSrc,
      nscript: scriptElement.getAttribute('data-nscript'),
    });

    // Update both property and attribute to ensure it works in all scenarios
    scriptElement.src = rewrittenSrc;
    scriptElement.setAttribute('src', rewrittenSrc);
  } else if (element.tagName === 'LINK') {
    const linkElement = element as HTMLLinkElement;
    const originalHref = linkElement.href || linkElement.getAttribute('href');
    if (!originalHref) return;

    const rewrittenHref = rewriteToFirstParty(originalHref);

    log.info('Next.js guard: rewriting Lockr SDK preload link', {
      original: originalHref,
      rewritten: rewrittenHref,
      rel: linkElement.getAttribute('rel'),
      as: linkElement.getAttribute('as'),
    });

    // Update both property and attribute to ensure it works in all scenarios
    linkElement.href = rewrittenHref;
    linkElement.setAttribute('href', rewrittenHref);
  }
}

/**
 * Install the Next.js guard to intercept dynamic script loading.
 * This patches Element.prototype.appendChild and insertBefore to catch
 * Next.js script elements and rewrite their URLs before insertion.
 */
export function installNextJsGuard(): void {
  // Prevent double installation
  if (guardInstalled) {
    log.debug('Next.js guard: already installed, skipping');
    return;
  }

  // Check if we're in a browser environment
  if (typeof window === 'undefined' || typeof Element === 'undefined') {
    log.debug('Next.js guard: not in browser environment, skipping');
    return;
  }

  log.info('Next.js guard: installing DOM interception for Lockr SDK');

  // Store original methods
  const originalAppendChild = Element.prototype.appendChild;
  const originalInsertBefore = Element.prototype.insertBefore;

  // Patch appendChild
  Element.prototype.appendChild = function <T extends Node>(this: Element, node: T): T {
    if (shouldRewriteElement(node)) {
      rewriteElement(node as HTMLScriptElement | HTMLLinkElement);
    }
    return originalAppendChild.call(this, node);
  };

  // Patch insertBefore
  Element.prototype.insertBefore = function <T extends Node>(
    this: Element,
    node: T,
    reference: Node | null
  ): T {
    if (shouldRewriteElement(node)) {
      rewriteElement(node as HTMLScriptElement | HTMLLinkElement);
    }
    return originalInsertBefore.call(this, node, reference);
  };

  guardInstalled = true;
  log.info('Next.js guard: DOM interception installed successfully');
}

/**
 * Check if the guard is currently installed.
 */
export function isGuardInstalled(): boolean {
  return guardInstalled;
}

/**
 * Reset the guard installation state (primarily for testing).
 */
export function resetGuardState(): void {
  guardInstalled = false;
}
