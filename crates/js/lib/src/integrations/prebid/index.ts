// Prebid.js bundle with prebidServerBidAdapter pre-configured for Trusted Server.
//
// The s2sConfig is automatically set to route all bid requests through /ad/auction
// which forwards to the configured Prebid Server. The server-side bidder list is
// managed by trusted-server.toml [integrations.prebid].bidders configuration.

import pbjs from 'prebid.js';
import 'prebid.js/modules/prebidServerBidAdapter.js';
import 'prebid.js/modules/consentManagementTcf.js';
import 'prebid.js/modules/consentManagementGpp.js';
import 'prebid.js/modules/consentManagementUsp.js';

import { log } from '../../core/log';

// s2sConfig routes all bid requests through Trusted Server's /ad/auction endpoint.
// The server enriches requests with synthetic IDs, geo data, and forwards to Prebid Server.
const TRUSTED_SERVER_S2S_CONFIG = {
  accountId: '1002',
  enabled: true,
  endpoint: '/ad/auction',
  adapter: 'prebidServer',
  timeout: 1000,
  // Allow bids from bidders not in the original request (e.g., mocktioneer in dev,
  // or when PBS returns bids from aliased/substituted bidders)
  allowUnknownBidderCodes: true,
};

// Store original methods for shimming
const originalRequestBids = pbjs.requestBids.bind(pbjs);

// Shim requestBids to dynamically update s2sConfig.bidders based on ad units.
// This ensures all bidders discovered in ad units are routed through server-side.
pbjs.requestBids = function (requestObj?: Parameters<typeof originalRequestBids>[0]) {
  log.debug('[tsjs-prebid] requestBids called');

  const opts = requestObj || {};
  const adUnits = opts.adUnits || pbjs.adUnits || [];

  // Collect all bidders from ad units
  const allBidders = new Set<string>();
  for (const unit of adUnits) {
    if (unit.bids) {
      for (const bid of unit.bids) {
        if (bid.bidder) {
          allBidders.add(bid.bidder);
        }
      }
    }
  }

  // Update s2sConfig with discovered bidders
  const updatedS2sConfig = {
    ...TRUSTED_SERVER_S2S_CONFIG,
    bidders: [...allBidders],
  };

  log.debug('[tsjs-prebid] Updating s2sConfig with bidders:', [...allBidders]);

  // Re-apply config with updated bidders list
  pbjs.setConfig({
    s2sConfig: updatedS2sConfig as unknown as Parameters<typeof pbjs.setConfig>[0]['s2sConfig'],
  });

  return originalRequestBids(opts);
};

// Initial configuration
pbjs.setConfig({
  debug: true,
  s2sConfig: TRUSTED_SERVER_S2S_CONFIG as Parameters<typeof pbjs.setConfig>[0]['s2sConfig'],
});

// IMPORTANT: When using prebid.js via NPM, processQueue() must be called explicitly
// after all modules are loaded. This:
// 1. Replaces que.push and cmd.push with async execution functions
// 2. Processes any commands already queued before prebid loaded
// Without this call, pbjs.que remains a plain array and queued commands never execute.
pbjs.processQueue();

log.debug('[tsjs-prebid] prebid initialized, processQueue called');

export { pbjs };
