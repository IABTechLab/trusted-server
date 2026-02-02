import { log } from '../../core/log';

/**
 * DataDome SDK Script Interception Guard
 *
 * Intercepts any dynamically inserted script tag that loads the DataDome SDK
 * and rewrites it to use the first-party domain proxy endpoint. This works
 * across all frameworks (Next.js, Nuxt, Gatsby, vanilla JS, etc.) and catches
 * scripts inserted via appendChild, insertBefore, or any other dynamic DOM
 * manipulation.
 *
 * Unlike Lockr/Permutive guards that use a fixed proxy path, the DataDome guard
 * preserves the original path from the DataDome URL (e.g., /tags.js, /js/check)
 * in the rewritten first-party URL.
 */

let installed = false;
const GUARD_NAME = 'DataDome';

/**
 * Check if a URL is a DataDome SDK URL.
 * Matches URLs where js.datadome.co is the host (not just a substring)
 */
function isDataDomeSdkUrl(url: string): boolean {
  if (!url) return false;

  const lower = url.toLowerCase();

  // Must match js.datadome.co as a domain, not as part of a filename
  // Valid patterns:
  // - https://js.datadome.co/...
  // - //js.datadome.co/...
  // - js.datadome.co/... (bare domain)
  // Invalid:
  // - https://cdn.example.com/js.datadome.co.js (domain is not js.datadome.co)
  return (
    lower.includes('://js.datadome.co/') ||
    (lower.includes('://js.datadome.co') && lower.endsWith('js.datadome.co')) ||
    lower.startsWith('//js.datadome.co/') ||
    (lower.startsWith('//js.datadome.co') && lower === '//js.datadome.co') ||
    lower.startsWith('js.datadome.co/') ||
    lower === 'js.datadome.co'
  );
}

/**
 * Extract the path from a DataDome URL to preserve it in the rewrite.
 * e.g., "https://js.datadome.co/tags.js" -> "/tags.js"
 *       "https://js.datadome.co/js/check" -> "/js/check"
 */
function extractDataDomePath(url: string): string {
  try {
    // Handle protocol-relative URLs
    let normalizedUrl = url;
    if (url.startsWith('//')) {
      normalizedUrl = 'https:' + url;
    } else if (!url.startsWith('http')) {
      normalizedUrl = 'https://' + url;
    }

    const parsed = new URL(normalizedUrl);
    // Return pathname + search (query string) if present
    return parsed.pathname + parsed.search;
  } catch {
    // Fallback: try to extract path after js.datadome.co
    const match = url.match(/js\.datadome\.co(\/[^'"]*)?/i);
    return match?.[1] || '/tags.js';
  }
}

/**
 * Build a first-party URL from the current page origin and the DataDome path.
 */
function rewriteDataDomeUrl(originalUrl: string): string {
  const protocol = window.location.protocol === 'https:' ? 'https' : 'http';
  const host = window.location.host;
  const path = extractDataDomePath(originalUrl);

  return `${protocol}://${host}/integrations/datadome${path}`;
}

/**
 * Check and rewrite a node if it's a DataDome script or preload link.
 */
function rewriteIfDataDome(node: Node): void {
  if (!node || !(node instanceof HTMLElement)) {
    return;
  }

  // Script elements
  if (node.tagName === 'SCRIPT') {
    const script = node as HTMLScriptElement;
    const src = script.src || script.getAttribute('src');
    if (src && isDataDomeSdkUrl(src)) {
      const rewritten = rewriteDataDomeUrl(src);
      log.info(`${GUARD_NAME} guard: rewriting dynamically inserted SDK script`, {
        original: src,
        rewritten,
      });
      script.src = rewritten;
      script.setAttribute('src', rewritten);
    }
    return;
  }

  // Link preload/prefetch elements
  if (node.tagName === 'LINK') {
    const link = node as HTMLLinkElement;
    const rel = link.getAttribute('rel');
    // Handle both preload and prefetch links for scripts
    if ((rel !== 'preload' && rel !== 'prefetch') || link.getAttribute('as') !== 'script') {
      return;
    }
    const href = link.href || link.getAttribute('href');
    if (href && isDataDomeSdkUrl(href)) {
      const rewritten = rewriteDataDomeUrl(href);
      log.info(`${GUARD_NAME} guard: rewriting SDK ${rel} link`, {
        original: href,
        rewritten,
      });
      link.href = rewritten;
      link.setAttribute('href', rewritten);
    }
  }
}

/**
 * Install the DataDome guard to intercept dynamic script loading.
 * Patches Element.prototype.appendChild and insertBefore to catch
 * ANY dynamically inserted DataDome SDK script elements and rewrite their URLs
 * before insertion. Works across all frameworks and vanilla JavaScript.
 *
 * Unlike the base script guard, this preserves the original path from the
 * DataDome URL (e.g., /tags.js, /js/check) in the rewritten URL.
 */
export function installDataDomeGuard(): void {
  if (installed) {
    log.debug(`${GUARD_NAME} guard: already installed, skipping`);
    return;
  }

  if (typeof window === 'undefined' || typeof Element === 'undefined') {
    log.debug(`${GUARD_NAME} guard: not in browser environment, skipping`);
    return;
  }

  log.info(`${GUARD_NAME} guard: installing DOM interception for SDK`);

  const originalAppendChild = Element.prototype.appendChild;
  const originalInsertBefore = Element.prototype.insertBefore;

  Element.prototype.appendChild = function <T extends Node>(this: Element, node: T): T {
    rewriteIfDataDome(node);
    return originalAppendChild.call(this, node) as T;
  };

  Element.prototype.insertBefore = function <T extends Node>(
    this: Element,
    node: T,
    reference: Node | null
  ): T {
    rewriteIfDataDome(node);
    return originalInsertBefore.call(this, node, reference) as T;
  };

  installed = true;
  log.info(`${GUARD_NAME} guard: DOM interception installed successfully`);
}

/**
 * Check if the guard is currently installed.
 */
export function isGuardInstalled(): boolean {
  return installed;
}

/**
 * Reset the guard installation state (primarily for testing).
 */
export function resetGuardState(): void {
  installed = false;
}

// Export for testing
export { isDataDomeSdkUrl, extractDataDomePath, rewriteDataDomeUrl };
