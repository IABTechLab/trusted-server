export function isArray<T>(v: unknown): v is T[] {
  return Array.isArray(v);
}

export function toArray<T>(v: T | T[]): T[] {
  return isArray<T>(v) ? v : [v];
}
