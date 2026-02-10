import { log } from '../../core/log';

/**
 * GPT Script Interception Guard
 *
 * Intercepts any dynamically inserted script or preload-link element whose
 * URL points at one of Google's ad-serving domains and rewrites it to the
 * first-party proxy, preserving the original path.
 *
 * Unlike the shared `createScriptGuard` factory (which maps all matching
 * URLs to a single fixed proxy path), this guard performs a *host swap*:
 *
 *   securepubads.g.doubleclick.net/pagead/managed/js/gpt/…/pubads_impl.js
 *   → publisher.com/integrations/gpt/pagead/managed/js/gpt/…/pubads_impl.js
 *
 * The server-side proxy serves script bodies verbatim, so this guard is
 * the sole mechanism that routes GPT's cascaded script loads (pubads_impl,
 * sub-modules, viewability, etc.) back through the first-party proxy.
 */

/** Google ad-serving domains whose scripts should be proxied. */
const GPT_DOMAINS = [
  'securepubads.g.doubleclick.net',
  'pagead2.googlesyndication.com',
  'googletagservices.com',
  'www.googletagservices.com',
] as const;

/** Integration route prefix on the first-party domain. */
const PROXY_PREFIX = '/integrations/gpt';

/**
 * Check if a URL belongs to one of Google's GPT / ad-serving domains.
 */
function isGptDomainUrl(url: string): boolean {
  if (!url) return false;
  const lower = url.toLowerCase();
  return GPT_DOMAINS.some((domain) => lower.includes(domain));
}

/**
 * Rewrite a GPT URL to the first-party proxy, preserving the path.
 *
 * ```
 * https://securepubads.g.doubleclick.net/pagead/managed/…
 * → https://publisher.com/integrations/gpt/pagead/managed/…
 * ```
 */
function rewriteUrl(originalUrl: string): string {
  try {
    const parsed = new URL(originalUrl);
    const protocol = window.location.protocol === 'https:' ? 'https' : 'http';
    const host = window.location.host;
    // Preserve path + query + fragment from the original URL.
    return `${protocol}://${host}${PROXY_PREFIX}${parsed.pathname}${parsed.search}${parsed.hash}`;
  } catch {
    // If URL parsing fails, fall back to simple string replacement.
    for (const domain of GPT_DOMAINS) {
      if (originalUrl.toLowerCase().includes(domain)) {
        return originalUrl.replace(
          new RegExp(`https?://(?:www\\.)?${domain.replace(/\./g, '\\.')}`, 'i'),
          `${window.location.protocol}//${window.location.host}${PROXY_PREFIX}`,
        );
      }
    }
    return originalUrl;
  }
}

/**
 * Determine whether a DOM node is a `<script>` or `<link rel="preload" as="script">`
 * whose URL points at a Google ad-serving domain.
 */
function shouldRewrite(node: Node): node is HTMLScriptElement | HTMLLinkElement {
  if (!node || !(node instanceof HTMLElement)) return false;

  if (node.tagName === 'SCRIPT') {
    const src = (node as HTMLScriptElement).src || node.getAttribute('src');
    return !!src && isGptDomainUrl(src);
  }

  if (node.tagName === 'LINK') {
    const link = node as HTMLLinkElement;
    if (link.getAttribute('rel') !== 'preload' || link.getAttribute('as') !== 'script') {
      return false;
    }
    const href = link.href || link.getAttribute('href');
    return !!href && isGptDomainUrl(href);
  }

  return false;
}

/**
 * Rewrite the URL attribute on a matched element.
 */
function rewriteElement(element: HTMLScriptElement | HTMLLinkElement): void {
  if (element.tagName === 'SCRIPT') {
    const script = element as HTMLScriptElement;
    const original = script.src || script.getAttribute('src') || '';
    const rewritten = rewriteUrl(original);

    log.info('GPT guard: rewriting dynamically inserted script', { original, rewritten });

    script.src = rewritten;
    script.setAttribute('src', rewritten);
  } else if (element.tagName === 'LINK') {
    const link = element as HTMLLinkElement;
    const original = link.href || link.getAttribute('href') || '';
    const rewritten = rewriteUrl(original);

    log.info('GPT guard: rewriting preload link', { original, rewritten });

    link.href = rewritten;
    link.setAttribute('href', rewritten);
  }
}

// -- Guard lifecycle --

let installed = false;

/**
 * Install the GPT guard to intercept dynamic script loading.
 *
 * Patches `Element.prototype.appendChild` and `Element.prototype.insertBefore`
 * to catch any dynamically inserted script elements whose URLs match Google's
 * ad-serving domains and rewrites them to the first-party proxy before they
 * are inserted into the DOM.
 */
export function installGptGuard(): void {
  if (installed) {
    log.debug('GPT guard: already installed, skipping');
    return;
  }

  if (typeof window === 'undefined' || typeof Element === 'undefined') {
    log.debug('GPT guard: not in browser environment, skipping');
    return;
  }

  log.info('GPT guard: installing DOM interception for Google ad scripts');

  const originalAppendChild = Element.prototype.appendChild;
  const originalInsertBefore = Element.prototype.insertBefore;

  Element.prototype.appendChild = function <T extends Node>(this: Element, node: T): T {
    if (shouldRewrite(node)) {
      rewriteElement(node as HTMLScriptElement | HTMLLinkElement);
    }
    return originalAppendChild.call(this, node) as T;
  };

  Element.prototype.insertBefore = function <T extends Node>(
    this: Element,
    node: T,
    reference: Node | null,
  ): T {
    if (shouldRewrite(node)) {
      rewriteElement(node as HTMLScriptElement | HTMLLinkElement);
    }
    return originalInsertBefore.call(this, node, reference) as T;
  };

  installed = true;
  log.info('GPT guard: DOM interception installed successfully');
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
