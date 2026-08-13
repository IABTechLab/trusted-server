import registryJson from './user_id_modules.json';

export interface PrebidUserIdModuleRegistryEntry {
  moduleName: string;
  configNames: string[];
  eidSources: string[];
  importPath: string;
  notes?: string;
}

interface PrebidUserIdModuleRegistryFile {
  defaultPreset: string[];
  modules: PrebidUserIdModuleRegistryEntry[];
}

export interface PrebidUserIdEidLike {
  source?: unknown;
  uids?: Array<{ id?: unknown; atype?: unknown; ext?: unknown }>;
}

export interface PrebidUserIdModuleResolution {
  modules: string[];
  missingSources: string[];
}

const registry = registryJson as PrebidUserIdModuleRegistryFile;

const DEFAULT_PREBID_USER_ID_MODULES = [...registry.defaultPreset];
export const PREBID_USER_ID_MODULE_REGISTRY = [...registry.modules];

const MODULE_SORT_ORDER = new Map(
  ['userId', ...DEFAULT_PREBID_USER_ID_MODULES].map((moduleName, index) => [moduleName, index])
);

function stableModuleSort(a: string, b: string): number {
  return (
    (MODULE_SORT_ORDER.get(a) ?? Number.MAX_SAFE_INTEGER) -
      (MODULE_SORT_ORDER.get(b) ?? Number.MAX_SAFE_INTEGER) || a.localeCompare(b)
  );
}

function normalizeSource(source: unknown): string | undefined {
  if (typeof source !== 'string') {
    return undefined;
  }
  const normalized = source.trim().toLowerCase();
  return normalized.length > 0 ? normalized : undefined;
}

function hasLiveIntentProvider(eid: PrebidUserIdEidLike): boolean {
  return Array.isArray(eid.uids)
    ? eid.uids.some((uid) => {
        const ext = uid.ext;
        return (
          ext !== null &&
          typeof ext === 'object' &&
          !Array.isArray(ext) &&
          (ext as Record<string, unknown>).provider === 'liveintent.com'
        );
      })
    : false;
}

export function knownUserIdConfigNames(): string[] {
  return [...new Set(PREBID_USER_ID_MODULE_REGISTRY.flatMap((entry) => entry.configNames))].sort();
}

export function resolvePrebidUserIdModulesFromEids(
  eids: PrebidUserIdEidLike[]
): PrebidUserIdModuleResolution {
  const modules = new Set<string>();
  const missingSources = new Set<string>();
  const modulesBySource = new Map<string, string>();

  for (const entry of PREBID_USER_ID_MODULE_REGISTRY) {
    for (const source of entry.eidSources) {
      modulesBySource.set(source.toLowerCase(), entry.moduleName);
    }
  }

  for (const eid of eids) {
    const source = normalizeSource(eid.source);
    if (!source) {
      continue;
    }

    const moduleName = modulesBySource.get(source);
    if (moduleName) {
      modules.add(moduleName);
      continue;
    }

    if (hasLiveIntentProvider(eid)) {
      modules.add('liveIntentIdSystem');
      continue;
    }

    missingSources.add(source);
  }

  if (modules.size > 0) {
    modules.add('userId');
  }

  return {
    modules: [...modules].sort(stableModuleSort),
    missingSources: [...missingSources].sort(),
  };
}
