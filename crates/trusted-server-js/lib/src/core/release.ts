declare const __TSJS_EMBEDDED_RELEASE_ID_V1__: string;
declare const __TSJS_EMBEDDED_INTEGRATION_IDS_V1__: readonly string[];

/** Build-stamped identity of the exact canonical production bundle set. */
export const EMBEDDED_RELEASE_ID = __TSJS_EMBEDDED_RELEASE_ID_V1__;

/** Build-generated inventory of every integration bundle admitted by this release. */
export const EMBEDDED_INTEGRATION_IDS = Object.freeze([
  ...__TSJS_EMBEDDED_INTEGRATION_IDS_V1__,
]);
