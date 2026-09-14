export interface BundleByteMeasurement {
  readonly rawBytes: number;
  readonly gzipBytes: number;
  readonly brotliBytes: number;
  readonly sha256: string;
}

export function measureBytes(value: Uint8Array): BundleByteMeasurement;
