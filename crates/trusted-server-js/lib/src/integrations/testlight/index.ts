import { EMBEDDED_RELEASE_ID } from '../../core/release';

import { createTestlightIntegrationRegistration } from './module';

if (typeof window !== 'undefined') {
  const register = (window.tsjs as unknown as { _registerIntegration?: unknown } | undefined)
    ?._registerIntegration;
  if (typeof register === 'function') {
    Reflect.apply(register, window.tsjs, [
      createTestlightIntegrationRegistration(EMBEDDED_RELEASE_ID),
    ]);
  }
}
