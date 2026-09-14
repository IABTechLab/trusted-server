const reflectApplyIntrinsic = Reflect.apply;
const textEncoder = new TextEncoder();
const textEncoderEncodeIntrinsic = TextEncoder.prototype.encode;

function unicodeScalarCount(value: string): number | undefined {
  let scalars = 0;
  for (let index = 0; index < value.length; index += 1) {
    const code = value.charCodeAt(index);
    if (code >= 0xd800 && code <= 0xdbff) {
      const next = value.charCodeAt(index + 1);
      if (!(next >= 0xdc00 && next <= 0xdfff)) return undefined;
      index += 1;
    } else if (code >= 0xdc00 && code <= 0xdfff) {
      return undefined;
    }
    scalars += 1;
  }
  return scalars;
}

function hasAsciiControl(value: string): boolean {
  for (let index = 0; index < value.length; index += 1) {
    const code = value.charCodeAt(index);
    if (code <= 0x1f || code === 0x7f) return true;
  }
  return false;
}

/** Validate a nonempty, well-formed UTF-16 string against byte and scalar bounds. */
export function validBoundedString(
  value: unknown,
  maximumBytes: number,
  options: { allowControls?: boolean; maximumScalars?: number } = {}
): value is string {
  if (typeof value !== 'string' || value.length === 0) return false;
  const scalarCount = unicodeScalarCount(value);
  return (
    scalarCount !== undefined &&
    (options.allowControls === true || !hasAsciiControl(value)) &&
    (reflectApplyIntrinsic(textEncoderEncodeIntrinsic, textEncoder, [value]) as Uint8Array)
      .length <= maximumBytes &&
    (options.maximumScalars === undefined || scalarCount <= options.maximumScalars)
  );
}
