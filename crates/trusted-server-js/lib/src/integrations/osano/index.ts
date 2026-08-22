export {
  disposeOsanoConsentMirror,
  initializeOsanoConsentMirror,
  mirrorOsanoConsent,
} from './consent_mirror';

import { EMBEDDED_RELEASE_ID } from '../../core/release';

import { createOsanoIntegrationRegistration } from './module';

if (typeof window !== 'undefined') {
  const register = (window.tsjs as unknown as { _registerIntegration?: unknown } | undefined)
    ?._registerIntegration;
  if (typeof register === 'function') {
    Reflect.apply(register, window.tsjs, [createOsanoIntegrationRegistration(EMBEDDED_RELEASE_ID)]);
  }
}
