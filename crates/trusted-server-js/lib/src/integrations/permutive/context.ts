import { EMBEDDED_RELEASE_ID } from '../../core/release';

import { createPermutiveIntegrationRegistration } from './module';

if (typeof window !== 'undefined') {
  const register = (window.tsjs as unknown as { _registerIntegration?: unknown } | undefined)
    ?._registerIntegration;
  if (typeof register === 'function') {
    Reflect.apply(register, window.tsjs, [
      createPermutiveIntegrationRegistration(EMBEDDED_RELEASE_ID),
    ]);
  }
}
