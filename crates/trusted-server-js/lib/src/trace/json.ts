import { traceObject, traceOwn, traceText } from './context';
import { traceItems } from './shape';
const MAX_STORED_BYTES = 512 * 1024;

/** Measures compact JSON using own data descriptors, never caller serialization. */
export function boundedJsonShape(
  value: unknown,
  maximumDepth = 10,
  maximumBytes = MAX_STORED_BYTES
): number | undefined {
  return traceJsonSnapshot(value, maximumDepth, maximumBytes)?.bytes;
}

/** Copies and measures the same bounded own JSON data, without caller get traps. */
export function traceJsonSnapshot(
  value: unknown,
  maximumDepth = 10,
  maximumBytes = MAX_STORED_BYTES
): { value: unknown; bytes: number } | undefined {
  const encoder = new TextEncoder();
  let bytes = 0;
  const ancestors = new Set<object>();
  const add = (text: string): boolean => {
    bytes += encoder.encode(text).length;
    return bytes <= maximumBytes;
  };
  const walk = (item: unknown, depth: number): { value: unknown } | undefined => {
    if (item === null || typeof item === 'boolean' || typeof item === 'number')
      return (typeof item !== 'number' || Number.isFinite(item)) && add(JSON.stringify(item))
        ? { value: item }
        : undefined;
    if (typeof item === 'string')
      return traceText(item, maximumBytes) && add(JSON.stringify(item))
        ? { value: item }
        : undefined;
    if (depth > maximumDepth || typeof item !== 'object' || ancestors.has(item)) return undefined;
    ancestors.add(item);
    let okay = true;
    let owned: unknown;
    if (Array.isArray(item)) {
      const array: unknown[] = [];
      owned = array;
      const items = traceItems(item, maximumBytes);
      if (!items || !add('[')) okay = false;
      else {
        for (let index = 0; index < items.length && okay; index += 1) {
          if (index !== 0 && !add(',')) {
            okay = false;
            break;
          }
          const child = walk(items[index], depth + 1);
          if (!child) {
            okay = false;
            break;
          }
          array.push(child.value);
        }
      }
      okay = okay && add(']');
    } else if (traceObject(item)) {
      const object: Record<string, unknown> = Object.create(null);
      owned = object;
      if (!add('{')) okay = false;
      const keys = Object.keys(item);
      for (let index = 0; index < keys.length && okay; index += 1) {
        const key = keys[index];
        const property = Object.getOwnPropertyDescriptor(item, key);
        okay =
          traceText(key, 128) &&
          (index === 0 || add(',')) &&
          add(JSON.stringify(key)) &&
          add(':') &&
          property !== undefined &&
          traceOwn(property, 'value');
        if (okay && property) {
          const child = walk(property.value, depth + 1);
          if (!child) okay = false;
          else object[key] = child.value;
        }
      }
      okay = okay && add('}');
    } else okay = false;
    ancestors.delete(item);
    return okay ? { value: owned } : undefined;
  };
  try {
    const owned = walk(value, 1);
    return owned ? { value: owned.value, bytes } : undefined;
  } catch {
    return undefined;
  }
}
