import { describe, expect, it, vi } from 'vitest';

import { createRelayConsentGuard } from '../../../src/integrations/prebid/relay_consent';

function setup(consent: Record<string, unknown>, mapping?: Record<string, unknown>) {
  let currentConsent = consent;
  let currentMapping = mapping;
  const setConfig = vi.fn((value: { consentManagement: Record<string, unknown> }) => {
    currentConsent = value.consentManagement;
  });
  const prepare = createRelayConsentGuard(
    'trustedServer',
    (key) => (key === 'consentManagement' ? currentConsent : currentMapping),
    setConfig
  );
  return {
    prepare,
    setConfig,
    consent: () => currentConsent,
    mapVendor: () => {
      currentMapping = { trustedServer: 1 };
    },
    replaceConsent: (value: Record<string, unknown>) => {
      currentConsent = value;
    },
  };
}

describe('first-party relay TCF configuration', () => {
  it('retains both default basic-ads gates and is idempotent', () => {
    const { prepare, setConfig, consent } = setup({ gdpr: { cmpApi: 'iab' } });

    prepare();
    prepare();

    expect(setConfig).toHaveBeenCalledTimes(1);
    expect(consent()).toEqual({
      gdpr: {
        cmpApi: 'iab',
        rules: [
          {
            purpose: 'basicAds',
            enforcePurpose: true,
            enforceVendor: true,
            softVendorExceptions: ['trustedServer'],
          },
        ],
      },
    });
  });

  it('preserves publisher settings, other rules, and existing exceptions without mutation', () => {
    const storage = { purpose: 'storage', enforcePurpose: true, enforceVendor: true };
    const basicAds = {
      purpose: 'basicAds',
      enforcePurpose: true,
      enforceVendor: true,
      vendorExceptions: ['exampleBidder'],
      softVendorExceptions: ['anotherBidder'],
      deferS2Sbidders: true,
    };
    const original = {
      gdpr: { cmpApi: 'iab', timeout: 500, rules: [storage, basicAds] },
      gpp: { cmpApi: 'iab' },
    };
    const { prepare, consent } = setup(original);

    prepare();

    expect(consent()).toEqual({
      ...original,
      gdpr: {
        ...original.gdpr,
        rules: [storage, { ...basicAds, softVendorExceptions: ['anotherBidder', 'trustedServer'] }],
      },
    });
    expect(basicAds.softVendorExceptions).toEqual(['anotherBidder']);
    expect(original.gdpr.rules).toEqual([storage, basicAds]);
  });

  it('leaves legacy TCF settings and their merge semantics unchanged', () => {
    const original = {
      cmpApi: 'static',
      consentData: { gdprApplies: true },
      strictStorageEnforcement: true,
    };
    const { prepare, consent, setConfig } = setup(original);

    prepare();

    expect(consent()).toEqual(original);
    expect(setConfig).not.toHaveBeenCalled();
  });

  it.each([{}, { gpp: { cmpApi: 'iab' } }, { gdpr: { enabled: false } }])(
    'does not activate GDPR collection for %j',
    (original) => {
      const { prepare, setConfig } = setup(original);

      prepare();

      expect(setConfig).not.toHaveBeenCalled();
    }
  );

  it('honors an explicitly configured relay GVL identity', () => {
    const { prepare, setConfig } = setup({ gdpr: { cmpApi: 'iab' } }, { trustedServer: 1 });

    prepare();

    expect(setConfig).not.toHaveBeenCalled();
  });

  it('prepares new publisher rules again before a later auction', () => {
    const { prepare, replaceConsent, consent } = setup({ gdpr: { cmpApi: 'iab' } });
    prepare();
    replaceConsent({
      strictStorageEnforcement: true,
      gdpr: {
        cmpApi: 'iab',
        rules: [
          { purpose: 'basicAds', enforcePurpose: true, softVendorExceptions: ['exampleBidder'] },
        ],
      },
    });

    prepare();

    expect(consent()).toMatchObject({
      strictStorageEnforcement: true,
      gdpr: {
        rules: [
          {
            purpose: 'basicAds',
            enforcePurpose: true,
            softVendorExceptions: ['exampleBidder', 'trustedServer'],
          },
        ],
      },
    });
  });

  it('retires its own exception when a publisher configures a GVL identity later', () => {
    const { prepare, mapVendor, consent } = setup({ gdpr: { cmpApi: 'iab' } });

    prepare();
    mapVendor();
    prepare();

    expect(consent()).toMatchObject({
      gdpr: {
        rules: [
          {
            purpose: 'basicAds',
            enforcePurpose: true,
            enforceVendor: true,
            softVendorExceptions: [],
          },
        ],
      },
    });
  });

  it('leaves malformed publisher rules unchanged', () => {
    const { prepare, setConfig } = setup({ gdpr: { cmpApi: 'iab', rules: 'invalid' } });

    prepare();

    expect(setConfig).not.toHaveBeenCalled();
  });
});
