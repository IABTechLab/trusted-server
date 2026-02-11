/**
 * Prebid.js integration module.
 * Handles initialization and configuration injection.
 */

interface PrebidConfig {
  accountId: string;
  enabled: boolean;
  bidders: string[];
  timeout: number;
  adapter: string;
  endpoint: string;
  syncEndpoint: string;
  cookieSet: boolean;
  cookiesetUrl: string;
  debug: boolean;
  adUnits?: unknown[];
}

interface Pbjs {
  que: (() => void)[];
  setConfig: (config: { s2sConfig: unknown; debug: boolean }) => void;
  addAdUnits: (units: unknown[]) => void;
}

declare global {
  interface Window {
    __tsjs_prebid?: PrebidConfig;
    pbjs?: Pbjs;
    __trustedServerPrebid?: boolean;
  }
}

export function init() {
  const config = window.__tsjs_prebid;
  if (!config) {
    return;
  }

  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const pbjs: Pbjs = (window.pbjs as any) || {};
  pbjs.que = pbjs.que || [];

  pbjs.que.push(() => {
    // Configure S2S (Server-to-Server)
    const s2sConfig = {
      accountId: config.accountId,
      enabled: config.enabled,
      bidders: config.bidders,
      timeout: config.timeout,
      adapter: config.adapter,
      endpoint: config.endpoint,
      syncEndpoint: config.syncEndpoint,
      cookieSet: config.cookieSet,
      cookiesetUrl: config.cookiesetUrl,
    };

    pbjs.setConfig({
      s2sConfig,
      debug: config.debug,
    });

    // Add Ad Units if provided
    if (config.adUnits && Array.isArray(config.adUnits)) {
      pbjs.addAdUnits(config.adUnits);
    }
  });

  window.pbjs = pbjs;
  window.__trustedServerPrebid = true;
}
