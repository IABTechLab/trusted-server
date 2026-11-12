import { log } from '../../core/log';

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

/**
 * Creates a per-auction TCF guard for a first-party auction relay.
 *
 * The relay has no GVL identity of its own. A basic-ads soft vendor exception
 * retains Prebid's purpose check while downstream auction endpoints receive
 * the original consent signals for their vendor-specific enforcement.
 */
export function createRelayConsentGuard(
  bidder: string,
  getConfig: (key: string) => unknown,
  setConfig: (config: { consentManagement: Record<string, unknown> }) => unknown
): () => void {
  let addedException = false;
  let applying = false;

  return () => {
    if (applying) return;
    try {
      const consent = getConfig('consentManagement');
      if (!isRecord(consent)) return;
      // tcfControl reads namespaced rules. Do not migrate publisher-owned
      // legacy settings: later mergeConfig calls must retain their semantics.
      const gdpr = consent.gdpr;
      if (!isRecord(gdpr) || gdpr.enabled === false) return;
      if (gdpr.rules !== undefined && !Array.isArray(gdpr.rules)) return;

      const mapping = getConfig('gvlMapping');
      const mapped = isRecord(mapping) && mapping[bidder] !== undefined;
      // An operator-supplied vendor identity keeps its vendor-consent gate.
      if (mapped && !addedException) return;

      const rules: unknown[] = gdpr.rules ?? [];
      let foundBasicAds = false;
      let changed = false;
      const nextRules = rules.map((rule) => {
        if (!isRecord(rule) || rule.purpose !== 'basicAds') return rule;
        foundBasicAds = true;
        const exceptions = rule.softVendorExceptions ?? [];
        if (!Array.isArray(exceptions)) return rule;
        const containsRelay = exceptions.includes(bidder);
        if (mapped ? !containsRelay : containsRelay) return rule;
        changed = true;
        return {
          ...rule,
          softVendorExceptions: mapped
            ? exceptions.filter((entry) => entry !== bidder)
            : [...exceptions, bidder],
        };
      });

      if (!foundBasicAds && !mapped) {
        // Custom rules replace Prebid's defaults, so retain both default gates.
        nextRules.push({
          purpose: 'basicAds',
          enforcePurpose: true,
          enforceVendor: true,
          softVendorExceptions: [bidder],
        });
        changed = true;
      }
      if (!changed) {
        if (mapped) addedException = false;
        return;
      }

      const nextGdpr = { ...gdpr, rules: nextRules };
      applying = true;
      addedException = !mapped;
      setConfig({
        consentManagement: { ...consent, gdpr: nextGdpr },
      });
    } catch (error) {
      log.error('[tsjs-prebid] auction relay TCF configuration could not be prepared', error);
    } finally {
      applying = false;
    }
  };
}
