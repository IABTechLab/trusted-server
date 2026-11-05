const omissionFailures = new WeakSet<object>();

export function omissionOverflow(): never {
  const error = new Error('omission_counter_overflow');
  omissionFailures.add(error);
  throw error;
}

/** Adds exact omissions without wrapping or saturating the public u16 counter. */
export function addOmissions(current: number, added: number): number {
  if (
    !Number.isInteger(current) ||
    !Number.isInteger(added) ||
    current < 0 ||
    added < 0 ||
    current > 65535 ||
    added > 65535 - current
  ) {
    omissionOverflow();
  }
  return current + added;
}

/** Recognizes only locally created overflow failures without inspecting caller errors. */
export function isOmissionOverflow(value: unknown): boolean {
  return typeof value === 'object' && value !== null && omissionFailures.has(value);
}
