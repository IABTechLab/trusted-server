declare const __TSJS_EMBEDDED_MAX_MANIFEST_MODULES_V1__: number;

/** Build-generated bounded manifest capacity without importing the product catalog. */
export const EMBEDDED_MAX_MANIFEST_MODULES =
  Number.isSafeInteger(__TSJS_EMBEDDED_MAX_MANIFEST_MODULES_V1__) &&
  __TSJS_EMBEDDED_MAX_MANIFEST_MODULES_V1__ >= 1 &&
  __TSJS_EMBEDDED_MAX_MANIFEST_MODULES_V1__ <= 256
    ? __TSJS_EMBEDDED_MAX_MANIFEST_MODULES_V1__
    : 0;
