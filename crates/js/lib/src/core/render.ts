// Rendering utilities for Trusted Server demo placements: find slots, seed placeholders,
// and inject creatives into sandboxed iframes.
import { log } from './log';
import type { AdUnit } from './types';
import { getUnit, getAllUnits, firstSize } from './registry';
import NORMALIZE_CSS from './styles/normalize.css?inline';
import IFRAME_TEMPLATE from './templates/iframe.html?raw';

function normalizeId(raw: string): string {
  const s = String(raw ?? '').trim();
  return s.startsWith('#') ? s.slice(1) : s;
}

// Locate an ad slot element by id, tolerating funky selectors provided by tag managers.
export function findSlot(id: string): HTMLElement | null {
  const nid = normalizeId(id);
  // Fast path
  const byId = document.getElementById(nid) as HTMLElement | null;
  if (byId) return byId;
  // Fallback for odd IDs (special chars) or if provided with quotes/etc.
  try {
    const selector = `[id="${nid.replace(/"/g, '\\"')}"]`;
    const byAttr = document.querySelector(selector) as HTMLElement | null;
    if (byAttr) return byAttr;
  } catch {
    // Ignore selector errors (e.g., invalid characters)
  }
  return null;
}

function ensureSlot(id: string): HTMLElement {
  const nid = normalizeId(id);
  let el = document.getElementById(nid) as HTMLElement | null;
  if (el) return el;
  el = document.createElement('div');
  el.id = nid;
  const body: HTMLElement | null = typeof document !== 'undefined' ? document.body : null;
  if (body && typeof body.appendChild === 'function') {
    body.appendChild(el);
  } else {
    // DOM not ready — attach once available
    const element = el;
    const onReady = () => {
      const readyBody = document.body;
      if (readyBody && !document.getElementById(nid) && element) readyBody.appendChild(element);
    };
    document.addEventListener('DOMContentLoaded', onReady, { once: true });
  }
  return el;
}

// Drop a placeholder message into the slot so pages don't sit empty pre-render.
export function renderAdUnit(codeOrUnit: string | AdUnit): void {
  const code = typeof codeOrUnit === 'string' ? codeOrUnit : codeOrUnit?.code;
  if (!code) return;
  const unit = typeof codeOrUnit === 'string' ? getUnit(code) : codeOrUnit;
  const size = (unit && firstSize(unit)) || [300, 250];
  const el = ensureSlot(code);
  try {
    el.textContent = `Trusted Server — ${size[0]}x${size[1]}`;
    log.info('renderAdUnit: rendered placeholder', { code, size });
  } catch {
    log.warn('renderAdUnit: failed', { code });
  }
}

// Render placeholders for every registered ad unit (used in simple publisher demos).
export function renderAllAdUnits(): void {
  try {
    const parentReady =
      typeof document !== 'undefined' && (document.body || document.documentElement);
    if (!parentReady) {
      log.warn('renderAllAdUnits: DOM not ready; skipping');
      return;
    }
    const units = getAllUnits();
    for (const u of units) {
      renderAdUnit(u);
    }
    log.info('renderAllAdUnits: rendered all placeholders', { count: units.length });
  } catch (e) {
    log.warn('renderAllAdUnits: failed', e as unknown);
  }
}

type IframeOptions = { name?: string; title?: string; width?: number; height?: number };

// Construct a sandboxed iframe sized for the ad so we can render arbitrary HTML.
export function createAdIframe(
  container: HTMLElement,
  opts: IframeOptions = {}
): HTMLIFrameElement {
  const iframe = document.createElement('iframe');
  // Attributes
  iframe.scrolling = 'no';
  iframe.frameBorder = '0';
  iframe.setAttribute('marginwidth', '0');
  iframe.setAttribute('marginheight', '0');
  if (opts.name) iframe.name = String(opts.name);
  iframe.title = opts.title || 'Ad content';
  iframe.setAttribute('aria-label', 'Advertisement');
  // Sandbox permissions for creatives
  try {
    iframe.sandbox.add(
      'allow-forms',
      'allow-popups',
      'allow-popups-to-escape-sandbox',
      'allow-same-origin',
      'allow-scripts',
      'allow-top-navigation-by-user-activation'
    );
  } catch (err) {
    log.debug('createAdIframe: sandbox add failed', err);
  }
  // Sizing + style
  const w = Math.max(0, Number(opts.width ?? 0) | 0);
  const h = Math.max(0, Number(opts.height ?? 0) | 0);
  if (w > 0) iframe.width = String(w);
  if (h > 0) iframe.height = String(h);
  const s = iframe.style;
  s.setProperty('border', '0');
  s.setProperty('margin', '0');
  s.setProperty('overflow', 'hidden');
  s.setProperty('display', 'block');
  if (w > 0) s.setProperty('width', `${w}px`);
  if (h > 0) s.setProperty('height', `${h}px`);
  // Insert into container
  container.appendChild(iframe);
  return iframe;
}

// Build a complete HTML document for a creative, suitable for use with iframe.srcdoc
export function buildCreativeDocument(creativeHtml: string): string {
  return IFRAME_TEMPLATE.replace('%NORMALIZE_CSS%', NORMALIZE_CSS).replace(
    '%CREATIVE_HTML%',
    creativeHtml
  );
}
