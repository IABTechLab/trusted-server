import path from 'node:path';

import { defineConfig } from 'vite';
import type { Plugin } from 'vite';

type BundleName = 'core' | 'ext' | 'creative';

type BundleConfig = {
  input: string;
  fileName: string;
  name: string;
  extend?: boolean;
};

const BUNDLES: Record<BundleName, BundleConfig> = {
  core: {
    input: path.resolve(__dirname, 'src/core/index.ts'),
    fileName: 'tsjs-core.js',
    name: 'tsjs',
  },
  ext: {
    input: path.resolve(__dirname, 'src/ext/index.ts'),
    fileName: 'tsjs-ext.js',
    name: 'tsjs',
    extend: true,
  },
  creative: {
    input: path.resolve(__dirname, 'src/creative/index.ts'),
    fileName: 'tsjs-creative.js',
    name: 'tscreative',
  },
};

function resolveBundleName(mode: string | undefined): BundleName {
  const fromEnv = process.env.TSJS_BUNDLE?.toLowerCase();
  if (fromEnv && isBundleName(fromEnv)) return fromEnv;

  const normalized = mode?.toLowerCase();
  if (normalized && isBundleName(normalized)) return normalized;
  return 'core';
}

function isBundleName(value: string): value is BundleName {
  return Object.hasOwn(BUNDLES, value);
}

export default defineConfig(({ mode }) => {
  const bundleName = resolveBundleName(mode);
  const bundle = BUNDLES[bundleName];
  const distDir = path.resolve(__dirname, '../dist');
  const buildTimestamp = new Date().toISOString();
  const banner = `// build: ${buildTimestamp}\n`;

  return {
    build: {
      emptyOutDir: false,
      outDir: distDir,
      assetsDir: '.',
      sourcemap: false,
      minify: 'esbuild',
      rollupOptions: {
        input: bundle.input,
        output: {
          format: 'iife',
          dir: distDir,
          entryFileNames: bundle.fileName,
          inlineDynamicImports: true,
          extend: bundle.extend ?? false,
          name: bundle.name,
        },
      },
    },
    plugins: [createTimestampBannerPlugin(banner)],
  };
});

function createTimestampBannerPlugin(banner: string): Plugin {
  return {
    name: 'tsjs-build-timestamp-banner',
    generateBundle(_options, bundleOutput) {
      for (const file of Object.values(bundleOutput)) {
        if (file.type === 'chunk') {
          file.code = `${banner}${file.code}`;
        } else if (file.type === 'asset' && typeof file.source === 'string') {
          file.source = `${banner}${file.source}`;
        }
      }
    },
  };
}
