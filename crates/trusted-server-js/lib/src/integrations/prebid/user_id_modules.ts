import registryJson from './user_id_modules.json';

export interface PrebidUserIdModuleRegistryEntry {
  moduleName: string;
  configNames: string[];
  notes?: string;
}

interface PrebidUserIdModuleRegistryFile {
  defaultPreset: string[];
  modules: PrebidUserIdModuleRegistryEntry[];
}

const registry = registryJson as PrebidUserIdModuleRegistryFile;

export const PREBID_USER_ID_MODULE_REGISTRY = [...registry.modules];

export function knownUserIdConfigNames(): string[] {
  return [...new Set(PREBID_USER_ID_MODULE_REGISTRY.flatMap((entry) => entry.configNames))].sort();
}

function findUserIdModuleEntry(configName: string): PrebidUserIdModuleRegistryEntry | undefined {
  const normalized = configName.toLowerCase();
  return PREBID_USER_ID_MODULE_REGISTRY.find((candidate) =>
    candidate.configNames.some((name) => name.toLowerCase() === normalized)
  );
}

/**
 * Returns every lowercased config name that addresses the same submodule as
 * `configName`, including `configName` itself.
 *
 * Prebid matches a `userSync.userIds` entry to a submodule on either its
 * `name` or its `aliasName`, case-insensitively, and then takes the first
 * matching entry (`modules/userId/index.ts`, `generateSubmoduleContainers`).
 * A registry entry's `configNames` is that full set for one module, so an
 * operator-managed name must claim all of them or a publisher entry naming an
 * alias would be retained ahead of it and win.
 */
export function userIdConfigNameAliases(configName: string): string[] {
  const entry = findUserIdModuleEntry(configName);
  if (!entry) return [configName.toLowerCase()];
  return entry.configNames.map((name) => name.toLowerCase());
}

/**
 * Returns a stable key for the Prebid submodule `configName` addresses.
 *
 * Prebid registers one submodule per registry entry and reaches it by that
 * entry's name or any of its aliases, so two names sharing an entry select the
 * same submodule and only the first configured entry is ever read. An
 * unregistered name addresses only itself, keyed on the lowercased spelling
 * Prebid's own lookup compares.
 */
export function userIdSubmoduleKey(configName: string): string {
  return findUserIdModuleEntry(configName)?.moduleName ?? configName.toLowerCase();
}
