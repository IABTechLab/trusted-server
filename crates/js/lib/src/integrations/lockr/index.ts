import { log } from '../../core/log';

import { installNextJsGuard } from './nextjs_guard';

// Type definition for Lockr global
declare const identityLockr: IdentityLockr | undefined;

interface IdentityLockr {
  host: string;
  app_id: string;
  expiryDateKeys: string[];
  firstPartyCookies: string[];
  canRefreshToken: boolean;
  macroDetectionEnabled: boolean;
  iluiMacroDetection: boolean;
  gdprApplies: boolean;
  consentString: string;
  gppString: string;
  ccpaString: string;
  isUTMTagsLoaded: boolean;
  isFirstPartyCookiesLoaded: boolean;
  allowedUTMTags: string[];
  lockrTrackingID: string;
  panoramaClientId: string;
  writeToDeviceConsentEUID: boolean;
  id5JSEnabled: boolean;
  firstIDPassHEM: boolean;
  panoramaPassHEM: boolean;
  firstIDEnabled: boolean;
  panoramaEnabled: boolean;
  isAdelphicEnabled: boolean;
  os: string;
  browser: string;
  country: string;
  city: string;
  latitude: string;
  longitude: string;
  ip: string;
  hashedUserAgent: string;
  tokenMappings: Record<string, string>;
  tokenSourceMappings: Record<string, string>;
  identitProvidersType: Record<string, string>;
  identityIdEncryptionSalt: string;
}

/**
 * Install the Lockr shim to rewrite API endpoints to first-party domain.
 * This function is called after the Lockr SDK has loaded and initialized.
 */
function installLockrShim() {
  log.info('Installing Lockr shim - rewriting API host to first-party domain');

  if (typeof identityLockr === 'undefined' || !identityLockr) {
    log.warn('Lockr shim: identityLockr global not found');
    return;
  }

  const host = window.location.host;
  const protocol = window.location.protocol === 'https:' ? 'https' : 'http';

  // Store original host for debugging
  const originalHost = identityLockr.host;

  // Rewrite to first-party domain
  // The Lockr SDK will now make all API calls through our proxy
  identityLockr.host = `${protocol}://${host}/integrations/lockr/api`;

  log.info('Lockr shim installed', {
    originalHost,
    newHost: identityLockr.host,
    appId: identityLockr.app_id,
  });
}

/**
 * Wait for Lockr SDK to be available before installing shim.
 * Polls for SDK availability with a maximum number of attempts.
 *
 * @param callback - Function to call when SDK is available
 * @param maxAttempts - Maximum number of polling attempts (default: 50)
 */
function waitForLockrSDK(callback: () => void, maxAttempts = 50) {
  let attempts = 0;

  const check = () => {
    attempts++;

    // Check if identityLockr global exists and is initialized with host
    if (typeof identityLockr !== 'undefined' && identityLockr && identityLockr.host) {
      log.info('Lockr SDK detected, installing shim');
      callback();
    } else if (attempts < maxAttempts) {
      // Check again in 50ms
      setTimeout(check, 50);
    } else {
      log.warn('Lockr SDK not detected after', maxAttempts * 50, 'ms');
    }
  };

  check();
}

if (typeof window !== 'undefined') {
  installNextJsGuard();

  waitForLockrSDK(() => installLockrShim());
}
