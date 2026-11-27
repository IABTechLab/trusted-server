/** Read an optional string-valued bid targeting map without coercing its values. */
export function parseBidTargeting(value: unknown): Record<string, string> | undefined {
  if (
    value === null ||
    typeof value !== 'object' ||
    Array.isArray(value) ||
    !Object.values(value).every((entry) => typeof entry === 'string')
  ) {
    return undefined;
  }
  return Object.fromEntries(Object.entries(value));
}
