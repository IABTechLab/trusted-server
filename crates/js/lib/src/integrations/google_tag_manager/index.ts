import { log } from '../../core/log';

import { installGtmGuard } from './script_guard';

/**
 * Google Tag Manager integration for tsjs
 *
 * Installs a script guard to intercept dynamically inserted GTM and Google
 * Analytics scripts and rewrites them to use the first-party proxy endpoint.
 *
 * The guard intercepts:
 * - Script elements with src containing www.googletagmanager.com
 * - Script elements with src containing www.google-analytics.com
 * - Link preload/prefetch elements for those scripts
 *
 * URLs are rewritten to preserve the original path:
 * - https://www.googletagmanager.com/gtm.js?id=GTM-XXXX -> /integrations/google_tag_manager/gtm.js?id=GTM-XXXX
 * - https://www.google-analytics.com/g/collect -> /integrations/google_tag_manager/g/collect
 */

if (typeof window !== 'undefined') {
  installGtmGuard();
  log.info('Google Tag Manager integration initialized');
}
