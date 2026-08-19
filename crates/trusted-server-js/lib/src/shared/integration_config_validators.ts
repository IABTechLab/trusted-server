/** Exact validator used by products whose browser projection is deliberately empty. */
export function isEmptyIntegrationConfigV1(candidate: unknown): boolean {
  try {
    return (
      typeof candidate === 'object' &&
      candidate !== null &&
      !Array.isArray(candidate) &&
      Object.getPrototypeOf(candidate) === Object.prototype &&
      Object.isFrozen(candidate) &&
      Reflect.ownKeys(candidate).length === 0
    );
  } catch {
    return false;
  }
}

function exactFrozenRecord(
  candidate: unknown,
  required: readonly string[],
  optional: readonly string[] = []
): Readonly<Record<string, unknown>> | undefined {
  try {
    if (
      typeof candidate !== 'object' ||
      candidate === null ||
      Array.isArray(candidate) ||
      Object.getPrototypeOf(candidate) !== Object.prototype ||
      !Object.isFrozen(candidate)
    ) {
      return undefined;
    }
    const keys = Reflect.ownKeys(candidate);
    if (
      keys.some((key) => typeof key !== 'string') ||
      required.some((key) => !keys.includes(key)) ||
      keys.some(
        (key) => typeof key !== 'string' || (!required.includes(key) && !optional.includes(key))
      )
    ) {
      return undefined;
    }
    for (const key of keys) {
      const descriptor = Object.getOwnPropertyDescriptor(candidate, key);
      if (!descriptor?.enumerable || !('value' in descriptor)) return undefined;
    }
    return candidate as Readonly<Record<string, unknown>>;
  } catch {
    return undefined;
  }
}

function frozenStringArray(candidate: unknown): candidate is readonly string[] {
  try {
    if (
      !Array.isArray(candidate) ||
      Object.getPrototypeOf(candidate) !== Array.prototype ||
      !Object.isFrozen(candidate) ||
      Reflect.ownKeys(candidate).length !== candidate.length + 1
    ) {
      return false;
    }
    for (let index = 0; index < candidate.length; index += 1) {
      const descriptor = Object.getOwnPropertyDescriptor(candidate, String(index));
      if (
        !descriptor?.enumerable ||
        !('value' in descriptor) ||
        typeof descriptor.value !== 'string'
      ) {
        return false;
      }
    }
    return true;
  } catch {
    return false;
  }
}

export function isGptIntegrationConfigV1(candidate: unknown): boolean {
  const config = exactFrozenRecord(candidate, ['gamAttributionEnabled']);
  return typeof config?.gamAttributionEnabled === 'boolean';
}

export function isDidomiIntegrationConfigV1(candidate: unknown): boolean {
  const config = exactFrozenRecord(candidate, ['proxyPath']);
  if (typeof config?.proxyPath !== 'string') return false;
  return (
    config.proxyPath.startsWith('/') &&
    !config.proxyPath.startsWith('//') &&
    !config.proxyPath.startsWith('/\\') &&
    !config.proxyPath.includes('?') &&
    !config.proxyPath.includes('#')
  );
}

export function isSourcepointIntegrationConfigV1(candidate: unknown): boolean {
  const config = exactFrozenRecord(candidate, ['rewriteSdk']);
  return typeof config?.rewriteSdk === 'boolean';
}

export function isPrebidIntegrationConfigV1(candidate: unknown): boolean {
  const config = exactFrozenRecord(
    candidate,
    ['accountId', 'timeout', 'debug', 'bidders'],
    ['clientSideBidders', 'excludedGamAdUnitPathSuffixes']
  );
  return Boolean(
    config &&
    typeof config.accountId === 'string' &&
    typeof config.timeout === 'number' &&
    Number.isInteger(config.timeout) &&
    config.timeout >= 0 &&
    config.timeout <= 4_294_967_295 &&
    typeof config.debug === 'boolean' &&
    frozenStringArray(config.bidders) &&
    (config.clientSideBidders === undefined || frozenStringArray(config.clientSideBidders)) &&
    (config.excludedGamAdUnitPathSuffixes === undefined ||
      frozenStringArray(config.excludedGamAdUnitPathSuffixes))
  );
}

export function isCreativeBootConfigV1(candidate: unknown): boolean {
  const config = exactFrozenRecord(candidate, ['version', 'enabled', 'clickGuard', 'renderGuard']);
  return Boolean(
    config &&
    config.version === 1 &&
    typeof config.enabled === 'boolean' &&
    typeof config.clickGuard === 'boolean' &&
    typeof config.renderGuard === 'boolean' &&
    (config.enabled || (!config.clickGuard && !config.renderGuard))
  );
}
