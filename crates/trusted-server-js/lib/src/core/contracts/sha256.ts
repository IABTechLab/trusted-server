const H: number[] = [];
const K: number[] = [];
const ENCODER = new TextEncoder();
for (let n = 2; K.length < 64; n += 1) {
  let prime = true;
  for (let d = 2; d * d <= n; d += 1) {
    if (n % d === 0) {
      prime = false;
      break;
    }
  }
  if (!prime) continue;
  if (H.length < 8) H.push((Math.sqrt(n) * 0x1_0000_0000) >>> 0);
  K.push((Math.cbrt(n) * 0x1_0000_0000) >>> 0);
}

function rotr(word: number, bits: number): number {
  return (word >>> bits) | (word << (32 - bits));
}

/** Calculate SHA-256 synchronously so boot validation remains effect-inert. */
export function sha256HexUtf8V1(text: string): string {
  const bytes = ENCODER.encode(text);
  const length = Math.ceil((bytes.length + 9) / 64) * 64;
  const data = new Uint8Array(length);
  data.set(bytes);
  data[bytes.length] = 0x80;
  const bits = bytes.length * 8;
  const view = new DataView(data.buffer);
  view.setUint32(length - 8, Math.floor(bits / 0x1_0000_0000));
  view.setUint32(length - 4, bits >>> 0);
  const state = [...H];
  const w = new Uint32Array(64);
  for (let offset = 0; offset < length; offset += 64) {
    for (let i = 0; i < 16; i += 1) w[i] = view.getUint32(offset + i * 4);
    for (let i = 16; i < 64; i += 1) {
      const left = w[i - 15]!;
      const right = w[i - 2]!;
      const s0 = rotr(left, 7) ^ rotr(left, 18) ^ (left >>> 3);
      const s1 = rotr(right, 17) ^ rotr(right, 19) ^ (right >>> 10);
      w[i] = (w[i - 16]! + s0 + w[i - 7]! + s1) >>> 0;
    }
    let [a, b, c, d, e, f, g, h] = state;
    for (let i = 0; i < 64; i += 1) {
      const s1 = rotr(e!, 6) ^ rotr(e!, 11) ^ rotr(e!, 25);
      const ch = (e! & f!) ^ (~e! & g!);
      const t1 = (h! + s1 + ch + K[i]! + w[i]!) >>> 0;
      const s0 = rotr(a!, 2) ^ rotr(a!, 13) ^ rotr(a!, 22);
      const maj = (a! & b!) ^ (a! & c!) ^ (b! & c!);
      const t2 = (s0 + maj) >>> 0;
      h = g;
      g = f;
      f = e;
      e = (d! + t1) >>> 0;
      d = c;
      c = b;
      b = a;
      a = (t1 + t2) >>> 0;
    }
    state[0] = (state[0]! + a!) >>> 0;
    state[1] = (state[1]! + b!) >>> 0;
    state[2] = (state[2]! + c!) >>> 0;
    state[3] = (state[3]! + d!) >>> 0;
    state[4] = (state[4]! + e!) >>> 0;
    state[5] = (state[5]! + f!) >>> 0;
    state[6] = (state[6]! + g!) >>> 0;
    state[7] = (state[7]! + h!) >>> 0;
  }
  return state.map((word) => word.toString(16).padStart(8, '0')).join('');
}

/** Measure UTF-8 with the same intrinsic used by the synchronous digest. */
export function utf8LengthV1(text: string): number {
  return ENCODER.encode(text).byteLength;
}
