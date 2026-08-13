import { describe, expect, it } from 'vitest';

import {
  knownUserIdConfigNames,
  resolvePrebidUserIdModulesFromEids,
} from '../../../src/integrations/prebid/user_id_modules';

const sampleEids = [
  { source: 'yahoo.com', uids: [{ id: 'connect-id', atype: 3 }] },
  { source: 'criteo.com', uids: [{ id: 'criteo-id', atype: 1 }] },
  { source: 'liveintent.com', uids: [{ id: 'liveintent-id', atype: 3 }] },
  {
    source: 'bidswitch.net',
    uids: [{ id: 'bidswitch-id', atype: 3, ext: { provider: 'liveintent.com' } }],
  },
  {
    source: 'liveintent.triplelift.com',
    uids: [{ id: 'triplelift-id', atype: 3, ext: { provider: 'liveintent.com' } }],
  },
  {
    source: 'rubiconproject.com',
    uids: [{ id: 'rubicon-id', atype: 3, ext: { provider: 'liveintent.com' } }],
  },
  {
    source: 'liveintent.indexexchange.com',
    uids: [{ id: 'index-id', atype: 3, ext: { provider: 'liveintent.com' } }],
  },
  {
    source: 'openx.net',
    uids: [{ id: 'openx-id', atype: 3, ext: { provider: 'liveintent.com' } }],
  },
  {
    source: 'pubmatic.com',
    uids: [{ id: 'pubmatic-id', atype: 3, ext: { provider: 'liveintent.com' } }],
  },
  {
    source: 'liveintent.sovrn.com',
    uids: [{ id: 'sovrn-id', atype: 3, ext: { provider: 'liveintent.com' } }],
  },
  {
    source: 'liveintent.unrulymedia.com',
    uids: [{ id: 'unruly-id', atype: 3, ext: { provider: 'liveintent.com' } }],
  },
  { source: 'pubcid.org', uids: [{ id: 'pubcid-id', atype: 1 }] },
  {
    source: 'adserver.org',
    uids: [{ id: 'tdid', atype: 1, ext: { rtiPartner: 'TDID' } }],
    inserter: 'adserver.org',
    matcher: 'adserver.org',
    mm: 4,
  },
];

describe('prebid user ID module registry', () => {
  it('resolves the target-site EID sample to deterministic Prebid user ID modules', () => {
    const result = resolvePrebidUserIdModulesFromEids(sampleEids);

    expect(result).toEqual({
      modules: [
        'userId',
        'connectIdSystem',
        'criteoIdSystem',
        'liveIntentIdSystem',
        'sharedIdSystem',
        'unifiedIdSystem',
      ],
      missingSources: [],
    });
  });

  it('exposes config names for modules that do not map EID sources', () => {
    expect(knownUserIdConfigNames()).toEqual(
      expect.arrayContaining(['lockrAIMId', 'pubProvidedId'])
    );
  });

  it('maps the Google PAIR EID source to pairIdSystem', () => {
    const result = resolvePrebidUserIdModulesFromEids([
      { source: 'google.com', uids: [{ id: 'pair-id' }] },
    ]);

    expect(result).toEqual({
      modules: ['userId', 'pairIdSystem'],
      missingSources: [],
    });
  });

  it('maps unknown LiveIntent provider-backed sources to liveIntentIdSystem', () => {
    const result = resolvePrebidUserIdModulesFromEids([
      {
        source: 'new-liveintent-partner.example',
        uids: [{ ext: { provider: 'liveintent.com' } }],
      },
    ]);

    expect(result).toEqual({
      modules: ['userId', 'liveIntentIdSystem'],
      missingSources: [],
    });
  });

  it('reports unknown non-LiveIntent sources', () => {
    const result = resolvePrebidUserIdModulesFromEids([
      { source: 'unknown.example', uids: [{ id: 'abc' }] },
    ]);

    expect(result).toEqual({ modules: [], missingSources: ['unknown.example'] });
  });
});
