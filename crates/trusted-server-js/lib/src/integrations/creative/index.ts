// Legacy callable helpers remain exported until Task 22; production performs
// only the release-bound integration registration below.
import { EMBEDDED_RELEASE_ID } from '../../core/release';
import type { TsCreativeConfig, TsCreativeApi } from '../../shared/globals';
import { creativeGlobal } from '../../shared/globals';

import { installClickGuard } from './click';
import { installDynamicImageProxy } from './image';
import { installDynamicIframeProxy } from './iframe';
import type { CreativeGuardHandle } from './startup';
import { createCreativeIntegrationRegistration } from './module';

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
let clickGuardHandle: CreativeGuardHandle | undefined;
let imageGuardHandle: CreativeGuardHandle | undefined;
let iframeGuardHandle: CreativeGuardHandle | undefined;

function applyConfig(): void {
  if (currentConfig.clickGuard && !clickGuardInstalled) {
    clickGuardHandle = installClickGuard();
    clickGuardInstalled = true;
  }

  if (currentConfig.renderGuard && !renderGuardInstalled) {
    imageGuardHandle = installDynamicImageProxy();
    iframeGuardHandle = installDynamicIframeProxy();
    renderGuardInstalled = true;
  }
}

/** Release only the wrappers, listeners, observers, and queued work installed by this module. */
export function disposeGuards(): void {
  const handles = [iframeGuardHandle, imageGuardHandle, clickGuardHandle];
  iframeGuardHandle = undefined;
  imageGuardHandle = undefined;
  clickGuardHandle = undefined;
  renderGuardInstalled = false;
  clickGuardInstalled = false;
  guardsInstallTriggered = false;
  for (let index = 0; index < handles.length; index += 1) {
    try {
      handles[index]?.dispose();
    } catch {
      // One hostile cleanup cannot retain another guard's owned state.
    }
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

export default tsCreative;

if (typeof window !== 'undefined') {
  const register = (window.tsjs as unknown as { _registerIntegration?: unknown } | undefined)
    ?._registerIntegration;
  if (typeof register === 'function') {
    Reflect.apply(register, window.tsjs, [
      createCreativeIntegrationRegistration(EMBEDDED_RELEASE_ID),
    ]);
  }
}
