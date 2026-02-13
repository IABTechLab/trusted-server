// Context provider registry: lets integrations contribute data to auction requests
// without core needing integration-specific knowledge.
import { log } from './log';

/**
 * A context provider returns key-value pairs to merge into the auction
 * request's `config` payload, or `undefined` to contribute nothing.
 */
export type ContextProvider = () => Record<string, unknown> | undefined;

const providers = new Map<string, ContextProvider>();

/**
 * Register a context provider that will be called before every auction request.
 * Integrations call this at import time to inject their data (e.g. segments,
 * identifiers) into the auction payload without core needing to know about them.
 *
 * Re-registering with the same `id` replaces the previous provider, preventing
 * duplicate accumulation in SPA environments.
 */
export function registerContextProvider(id: string, provider: ContextProvider): void {
  providers.set(id, provider);
  log.debug('context: registered provider', { id, total: providers.size });
}

/**
 * Collect context from all registered providers. Called by core's `requestAds`
 * to build the `config` object sent to `/auction`.
 *
 * Each provider's returned keys are merged (later providers win on collision).
 * Providers that throw or return `undefined` are silently skipped.
 */
export function collectContext(): Record<string, unknown> {
  const context: Record<string, unknown> = {};
  for (const provider of providers.values()) {
    try {
      const data = provider();
      if (data) Object.assign(context, data);
    } catch {
      log.debug('context: provider threw, skipping');
    }
  }
  return context;
}
