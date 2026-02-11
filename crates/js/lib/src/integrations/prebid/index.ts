/**
 * Prebid.js integration module.
 * Handles initialization and configuration injection.
 */

declare global {
    interface Window {
        __tsjs_prebid?: any;
        pbjs?: any;
        __trustedServerPrebid?: boolean;
    }
}

export function init() {
    const config = window.__tsjs_prebid;
    if (!config) {
        return;
    }

    const pbjs = window.pbjs || {};
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
