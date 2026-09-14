/** @file Installs and configures the global creative protection runtime. */
import { log } from '../../core/log';
import type { TsCreativeConfig, CreativeWindow, TsCreativeApi } from '../../shared/globals';
import { creativeGlobal, resolveWindow } from '../../shared/globals';

import { installClickGuard } from './click';
import { installDynamicImageProxy } from './image';
import { installDynamicIframeProxy } from './iframe';

/** Re-export the image source guard for explicit installation. */
export { installDynamicImageProxy } from './image';
/** Re-export the iframe source guard for explicit installation. */
export { installDynamicIframeProxy } from './iframe';

const DEFAULT_CONFIG: Required<TsCreativeConfig> = {
  clickGuard: true,
  renderGuard: false,
};

let currentConfig: Required<TsCreativeConfig> = { ...DEFAULT_CONFIG };
let guardsInstallTriggered = false;
let clickGuardInstalled = false;
let renderGuardInstalled = false;

function applyConfig(): void {
  if (currentConfig.clickGuard && !clickGuardInstalled) {
    installClickGuard();
    clickGuardInstalled = true;
  }

  if (currentConfig.renderGuard && !renderGuardInstalled) {
    installDynamicImageProxy();
    installDynamicIframeProxy();
    renderGuardInstalled = true;
  }
}

function mergeConfig(cfg: TsCreativeConfig): void {
  currentConfig = {
    clickGuard: cfg.clickGuard ?? currentConfig.clickGuard,
    renderGuard: cfg.renderGuard ?? currentConfig.renderGuard,
  };
  creativeGlobal.tsCreativeConfig = { ...currentConfig };
}

/** Merge creative guard configuration and activate newly enabled guards. */
export function setCreativeConfig(cfg: TsCreativeConfig): void {
  mergeConfig(cfg);
  if (guardsInstallTriggered) {
    applyConfig();
  }
}

/** Return a copy of the effective creative guard configuration. */
export function getCreativeConfig(): TsCreativeConfig {
  return { ...currentConfig };
}

/** Install the enabled click and dynamic-render guards at most once per page. */
export function installGuards(): void {
  if (!guardsInstallTriggered) {
    guardsInstallTriggered = true;
  }
  applyConfig();
}

/** Public creative guard API exposed as `globalThis.tscreative`. */
export const tsCreative: TsCreativeApi = {
  installGuards,
  setConfig: setCreativeConfig,
  getConfig: getCreativeConfig,
};

try {
  creativeGlobal.tscreative = tsCreative;
} catch (err) {
  log.debug('tsjs-creative: failed to expose global tscreative', err);
}

/** Default creative-runtime API export. */
export default tsCreative;

(function auto() {
  // Auto-install on load so publishers just reference the bundle.
  const maybeWindow = resolveWindow();
  if (!maybeWindow || typeof document === 'undefined') return;

  const win = maybeWindow as CreativeWindow;
  const initialConfig = creativeGlobal.tsCreativeConfig ?? win.tsCreativeConfig;
  if (initialConfig) {
    mergeConfig(initialConfig);
  } else {
    creativeGlobal.tsCreativeConfig = { ...currentConfig };
  }
  if (win.__ts_creative_installed) return;
  win.__ts_creative_installed = true;

  installGuards();

  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', () => installGuards());
  }
})();
