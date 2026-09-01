import { EMBEDDED_RELEASE_ID } from '../../core/release';

import { createDidomiIntegrationRegistration } from './module';

if (typeof window !== 'undefined') {
  const register = (window.tsjs as unknown as { _registerIntegration?: unknown } | undefined)
    ?._registerIntegration;
  if (typeof register === 'function') {
    Reflect.apply(register, window.tsjs, [
      createDidomiIntegrationRegistration(EMBEDDED_RELEASE_ID),
    ]);
  }
}
