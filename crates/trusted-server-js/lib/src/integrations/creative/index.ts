// Entry point for the creative runtime: wires up click + image + iframe guards globally.
import { log } from '../../core/log';
import type { TsCreativeConfig, CreativeWindow, TsCreativeApi } from '../../shared/globals';
import { creativeGlobal, resolveWindow } from '../../shared/globals';

import { installClickGuard } from './click';
import { installDynamicImageProxy } from './image';
import { installDynamicIframeProxy } from './iframe';

export { installDynamicImageProxy } from './image';
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

export function setCreativeConfig(cfg: TsCreativeConfig): void {
  mergeConfig(cfg);
  if (guardsInstallTriggered) {
    applyConfig();
  }
}

export function getCreativeConfig(): TsCreativeConfig {
  return { ...currentConfig };
}

// Public entry for creative runtime: install click + image protections once per page.
export function installGuards(): void {
  if (!guardsInstallTriggered) {
    guardsInstallTriggered = true;
  }
  applyConfig();
}

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
