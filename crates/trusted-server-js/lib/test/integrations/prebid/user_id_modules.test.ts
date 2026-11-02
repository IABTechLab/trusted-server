import { describe, expect, it } from 'vitest';

import {
  knownUserIdConfigNames,
  userIdSubmoduleKey,
} from '../../../src/integrations/prebid/user_id_modules';
import registry from '../../../src/integrations/prebid/user_id_modules.json';

describe('prebid user ID module registry', () => {
  it('exposes config names for modules that do not map EID sources', () => {
    expect(knownUserIdConfigNames()).toEqual(
      expect.arrayContaining(['lockrAIMId', 'pubProvidedId'])
    );
  });

  it('includes identityLinkIdSystem in the checked-in default preset', () => {
    expect(registry.defaultPreset).toContain('identityLinkIdSystem');
  });

  it('keys every alias of one module on the same submodule', () => {
    expect(userIdSubmoduleKey('pubCommonId')).toBe(userIdSubmoduleKey('sharedId'));
    expect(userIdSubmoduleKey('SHAREDID')).toBe(userIdSubmoduleKey('sharedId'));
    expect(userIdSubmoduleKey('sharedId')).toBe('sharedIdSystem');
  });

  it('keys distinct modules and unregistered names apart', () => {
    expect(userIdSubmoduleKey('identityLink')).not.toBe(userIdSubmoduleKey('sharedId'));
    // An unregistered name addresses only itself, keyed the way Prebid compares.
    expect(userIdSubmoduleKey('NotARegisteredModule')).toBe('notaregisteredmodule');
  });

  it('carries no EID source metadata', () => {
    expect(registry.modules.length, 'the registry should still list modules').toBeGreaterThan(0);
    expect(
      registry.modules.filter((entry) => 'eidSources' in entry).map((entry) => entry.moduleName),
      'no entry should keep eidSources'
    ).toEqual([]);
  });

  it('keeps submodule keys and config names unique across registry entries', () => {
    // Actual registrations are checked by prebid-user-id-registry.test.mjs;
    // these checks prevent separate entries from sharing an ownership key.
    const moduleNames = registry.modules.map((entry) => entry.moduleName);
    expect(moduleNames).toEqual([...new Set(moduleNames)]);

    const configNames = registry.modules.flatMap((entry) =>
      entry.configNames.map((name) => name.toLowerCase())
    );
    expect(configNames).toEqual([...new Set(configNames)]);

    // The unregistered-name fallback keys on the lowercased spelling, so no
    // module name may collide with it.
    expect(moduleNames.filter((name) => name === name.toLowerCase())).toEqual([]);
  });
});
