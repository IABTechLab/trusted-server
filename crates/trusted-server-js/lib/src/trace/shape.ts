import { traceInteger, traceOwn } from './context';

/** Accepts a dense bounded JSON array with no added fields or accessors. */
export function traceArray(value: unknown, limit: number): value is unknown[] {
  return traceItems(value, limit) !== undefined;
}

/** Copies bounded own array data without using caller methods or iterators. */
export function traceItems(value: unknown, limit: number): unknown[] | undefined {
  if (!Array.isArray(value) || Object.getPrototypeOf(value) !== Array.prototype) return undefined;
  const length = Object.getOwnPropertyDescriptor(value, 'length')?.value;
  if (!traceInteger(length, limit)) return undefined;
  const keys = Reflect.ownKeys(value);
  if (keys.length !== length + 1) return undefined;
  if (
    !keys.every((key) => {
      if (key === 'length') return true;
      if (typeof key !== 'string' || !/^(?:0|[1-9]\d*)$/.test(key) || Number(key) >= length)
        return false;
      const property = Object.getOwnPropertyDescriptor(value, key);
      return property?.enumerable === true && traceOwn(property, 'value');
    })
  )
    return undefined;
  const owned: unknown[] = [];
  for (let index = 0; index < length; index += 1) {
    const property = Object.getOwnPropertyDescriptor(value, String(index));
    if (!property || property.enumerable !== true || !traceOwn(property, 'value')) return undefined;
    owned.push(property.value);
  }
  return owned;
}
