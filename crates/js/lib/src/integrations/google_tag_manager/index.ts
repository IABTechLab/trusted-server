import { log } from '../../core/log';

import { installGtmBeaconGuard } from './script_guard';
import { installGtmGuard } from './script_guard';

/**
 * Google Tag Manager integration for tsjs
 *
 * Installs guards to intercept GTM and Google Analytics traffic:
 *
 * 1. **Script guard** — intercepts dynamically inserted `<script>` and
 *    `<link>` elements and rewrites their URLs to the first-party proxy.
 *
 * 2. **Beacon guard** — intercepts `navigator.sendBeacon()` and `fetch()`
 *    calls to Google Analytics domains (www.google-analytics.com,
 *    analytics.google.com) and rewrites them to the first-party proxy.
 *    This is necessary because gtag.js constructs beacon URLs dynamically
 *    from bare domain strings, which can't be safely rewritten at the
 *    script level.
 *
 * URLs are rewritten to preserve the original path:
 * - https://www.googletagmanager.com/gtm.js?id=GTM-XXXX -> /integrations/google_tag_manager/gtm.js?id=GTM-XXXX
 * - https://www.google-analytics.com/g/collect -> /integrations/google_tag_manager/g/collect
 * - https://analytics.google.com/g/collect -> /integrations/google_tag_manager/g/collect
 */

if (typeof window !== 'undefined') {
  installGtmGuard();
  installGtmBeaconGuard();
  log.info('Google Tag Manager integration initialized');
}
