declare const __TSJS_EMBEDDED_INTEGRATION_IDS_V1__: readonly string[];
declare const __TSJS_EMBEDDED_RUNTIME_CATALOG_V1__: readonly Readonly<{
  id: string;
  phase: 'takeover' | 'deferred';
  trigger: 'first_display_or_idle' | null;
  config: import('../kernel/release_catalog').ReleaseConfigSourceV1;
  consumes: readonly string[];
  provides: readonly string[];
}>[];

export { EMBEDDED_RELEASE_ID } from './release_id';

/** Build-generated inventory of every integration bundle admitted by this release. */
export const EMBEDDED_INTEGRATION_IDS = Object.freeze([...__TSJS_EMBEDDED_INTEGRATION_IDS_V1__]);

/** Build-generated capability/order authority without build-only product prose. */
export const EMBEDDED_RUNTIME_CATALOG = Object.freeze(
  __TSJS_EMBEDDED_RUNTIME_CATALOG_V1__.map((entry) =>
    Object.freeze({
      id: entry.id,
      phase: entry.phase,
      trigger: entry.trigger,
      config: entry.config,
      consumes: Object.freeze([...entry.consumes]),
      provides: Object.freeze([...entry.provides]),
    })
  )
);
