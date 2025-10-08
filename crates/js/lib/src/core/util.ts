// Tiny shared helpers used across core modules.
export function isArray<T>(v: unknown): v is T[] {
  return Array.isArray(v);
}

// Normalise a single value into an array for simple iteration helpers.
export function toArray<T>(v: T | T[]): T[] {
  return isArray<T>(v) ? v : [v];
}
