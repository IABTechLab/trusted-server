// Entry point for the creative runtime: wires up click + image + iframe guards globally.
import { log } from '../core/log';
import type { CreativeWindow, TsCreativeApi } from '../shared/globals';
import { creativeGlobal, resolveWindow } from '../shared/globals';

import { installClickGuard } from './click';
import { installDynamicImageProxy } from './image';
import { installDynamicIframeProxy } from './iframe';

export { installDynamicImageProxy } from './image';
export { installDynamicIframeProxy } from './iframe';

let guardsInstalled = false;

// Public entry for creative runtime: install click + image protections once per page.
export function installGuards(): void {
  if (guardsInstalled) return;
  guardsInstalled = true;
  installClickGuard();
  installDynamicImageProxy();
  installDynamicIframeProxy();
}

export const tsCreative: TsCreativeApi = { installGuards };

try {
  creativeGlobal.tscreative = tsCreative;
} catch (err) {
  log.debug('tsjs-creative: failed to expose global tscreative', err);
}

export default tsCreative;

(function auto() {
  // Auto-install on load so publishers just reference the bundle.
  const maybeWindow = resolveWindow();
  if (!maybeWindow || typeof document === 'undefined') return;

  const win = maybeWindow as CreativeWindow;
  if (win.__ts_creative_installed) return;
  win.__ts_creative_installed = true;

  installGuards();

  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', () => installGuards());
  }
})();
