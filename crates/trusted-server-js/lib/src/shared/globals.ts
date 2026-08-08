/** Minimal cross-runtime access to optional browser storage. */
export const creativeGlobal = globalThis as typeof globalThis & {
  localStorage?: Storage;
};
