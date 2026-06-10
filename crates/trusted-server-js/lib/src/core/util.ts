// Normalise a single value into an array for simple iteration helpers.
export function toArray<T>(v: T | T[]): T[] {
  return Array.isArray(v) ? v : [v];
}
