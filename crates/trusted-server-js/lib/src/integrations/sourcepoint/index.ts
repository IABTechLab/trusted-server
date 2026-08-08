import { EMBEDDED_RELEASE_ID } from '../../core/release';

import { createSourcepointIntegrationRegistration } from './module';

export {
  disposeSourcepointConsentMirror,
  initializeSourcepointConsentMirror,
  mirrorSourcepointConsent,
} from './consent_mirror';

if (typeof window !== 'undefined') {
  const register = (window.tsjs as unknown as { _registerIntegration?: unknown } | undefined)
    ?._registerIntegration;
  if (typeof register === 'function') {
    Reflect.apply(register, window.tsjs, [
      createSourcepointIntegrationRegistration(EMBEDDED_RELEASE_ID),
    ]);
  }
}
