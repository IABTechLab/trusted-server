import { log } from './log';
import type { AdUnit } from './types';
import { getUnit, getAllUnits, firstSize } from './registry';
import { getConfig } from './config';

function normalizeId(raw: string): string {
  const s = String(raw ?? '').trim();
  return s.startsWith('#') ? s.slice(1) : s;
}

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
  } catch {}
  return null;
}

function ensureSlot(id: string): HTMLElement {
  const nid = normalizeId(id);
  let el = document.getElementById(nid) as HTMLElement | null;
  if (el) return el;
  el = document.createElement('div');
  el.id = nid;
  const body: HTMLElement | null = (document as any).body || null;
  if (body && typeof body.appendChild === 'function') {
    body.appendChild(el);
  } else {
    // DOM not ready — attach once available
    document.addEventListener(
      'DOMContentLoaded',
      () => {
        const b = (document as any).body as HTMLElement | undefined;
        if (b && !document.getElementById(nid)) b.appendChild(el!);
      },
      { once: true }
    );
  }
  return el;
}

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

export function renderAllAdUnits(): void {
  try {
    const parentReady =
      typeof document !== 'undefined' &&
      ((document as any).body || (document as any).documentElement);
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

export function renderCreativeIntoSlot(slotId: string, html: string): void {
  const existing = findSlot(slotId);
  const cfg = getConfig?.() || {};
  const createIfMissing = (cfg as any).autoCreateSlots === true;
  if (!existing && !createIfMissing) {
    log.warn('renderCreativeIntoSlot: slot not found; skipping render', { slotId });
    return;
  }
  const el = existing ?? ensureSlot(slotId);
  if (!existing && createIfMissing) {
    log.warn('renderCreativeIntoSlot: slot not found; created container', { slotId });
  }
  try {
    // Clear previous content
    el.innerHTML = '';
    // Determine size if available
    const unit = getUnit(slotId);
    const sz = (unit && firstSize(unit)) || [300, 250];
    const iframe = createAdIframe(el, {
      name: `tsjs_iframe_${slotId}`,
      title: 'Ad content',
      width: sz[0],
      height: sz[1],
    });
    writeHtmlToIframe(iframe, html);
    log.info('renderCreativeIntoSlot: rendered', { slotId, width: sz[0], height: sz[1] });
  } catch (err) {
    log.warn('renderCreativeIntoSlot: failed', { slotId, err });
  }
}

// Minimal normalize CSS to reset default margins and typography inside the iframe
const NORMALIZE_CSS =
  '/*! normalize.css v8.0.1 | MIT License | github.com/necolas/normalize.css */button,hr,input{overflow:visible}progress,sub,sup{vertical-align:baseline}[type=checkbox],[type=radio],legend{box-sizing:border-box;padding:0}html{line-height:1.15;-webkit-text-size-adjust:100%}body{margin:0}details,main{display:block}h1{font-size:2em;margin:.67em 0}hr{box-sizing:content-box;height:0}code,kbd,pre,samp{font-family:monospace,monospace;font-size:1em}a{background-color:transparent}abbr[title]{border-bottom:none;text-decoration:underline;text-decoration:underline dotted}b,strong{font-weight:bolder}small{font-size:80%}sub,sup{font-size:75%;line-height:0;position:relative}sub{bottom:-.25em}sup{top:-.5em}img{border-style:none}button,input,optgroup,select,textarea{font-family:inherit;font-size:100%;line-height:1.15;margin:0}button,select{text-transform:none}[type=button],[type=reset],[type=submit],button{-webkit-appearance:button}[type=button]::-moz-focus-inner,[type=reset]::-moz-focus-inner,[type=submit]::-moz-focus-inner,button::-moz-focus-inner{border-style:none;padding:0}[type=button]:-moz-focusring,[type=reset]:-moz-focusring,[type=submit]:-moz-focusring,button:-moz-focusring{outline:ButtonText dotted 1px}fieldset{padding:.35em .75em .625em}legend{color:inherit;display:table;max-width:100%;white-space:normal}textarea{overflow:auto}[type=number]::-webkit-inner-spin-button,[type=number]::-webkit-outer-spin-button{height:auto}[type=search]{-webkit-appearance:textfield;outline-offset:-2px}[type=search]::-webkit-search-decoration{-webkit-appearance:none}::-webkit-file-upload-button{-webkit-appearance:button;font:inherit}summary{display:list-item}[hidden],template{display:none}';

type IframeOptions = { name?: string; title?: string; width?: number; height?: number };

function createAdIframe(container: HTMLElement, opts: IframeOptions = {}): HTMLIFrameElement {
  const iframe = document.createElement('iframe');
  // Attributes
  iframe.scrolling = 'no';
  iframe.frameBorder = '0';
  (iframe as any).marginWidth = '0';
  (iframe as any).marginHeight = '0';
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
      'allow-top-navigation-by-user-activation',
    );
  } catch {}
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

function writeHtmlToIframe(iframe: HTMLIFrameElement, creativeHtml: string): void {
  try {
    const doc = (iframe.contentDocument || iframe.contentWindow?.document) as Document | undefined;
    if (!doc) return;
    // Build full HTML with normalize CSS to avoid default body margins
    const html = `<!DOCTYPE html><html><head><meta charset="utf-8"><style>${NORMALIZE_CSS}</style></head><body style="margin:0;padding:0;overflow:hidden">${creativeHtml}</body></html>`;
    doc.open();
    doc.write(html);
    doc.close();
  } catch (err) {
    log.warn('renderCreativeIntoSlot: iframe write failed', { err });
  }
}
