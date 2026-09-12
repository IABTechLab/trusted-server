import { describe, expectTypeOf, it } from 'vitest';

import type {
  AddAdUnitsResult,
  ProgrammaticAdUnit,
  RequestAdsOptions,
  RequestAdsResult,
  TsjsApi,
  TsjsCommandQueue,
  TsjsDiagnostics,
  TsjsLog,
} from '../../src';

describe('public hard-cutover types', () => {
  it('exports the exact Promise API without legacy helper names', () => {
    type ExpectedKeys =
      | 'version'
      | 'releaseId'
      | 'boot'
      | 'que'
      | 'log'
      | '_registerIntegration'
      | 'addAdUnits'
      | 'requestAds'
      | 'diagnostics'
      | '_internal';

    expectTypeOf<keyof TsjsApi>().toEqualTypeOf<ExpectedKeys>();
    expectTypeOf<TsjsApi['version']>().toEqualTypeOf<'1.0.0'>();
    expectTypeOf<TsjsApi['que']>().toEqualTypeOf<TsjsCommandQueue>();
    expectTypeOf<TsjsApi['log']>().toEqualTypeOf<TsjsLog>();
    expectTypeOf<Extract<TsjsApi, { diagnostics: TsjsDiagnostics }>['diagnostics']>().toEqualTypeOf<
      Readonly<TsjsDiagnostics>
    >();
    expectTypeOf<TsjsApi['addAdUnits']>().parameters.toEqualTypeOf<
      [ProgrammaticAdUnit | readonly ProgrammaticAdUnit[]]
    >();
    expectTypeOf<TsjsApi['addAdUnits']>().returns.toEqualTypeOf<AddAdUnitsResult>();
    expectTypeOf<TsjsApi['requestAds']>().toEqualTypeOf<
      (options?: RequestAdsOptions) => Promise<RequestAdsResult>
    >();
  });
});
