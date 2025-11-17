import fs from 'node:fs';
import path from 'node:path';

import { defineConfig } from 'vite';
import type { Plugin } from 'vite';

type BundleConfig = {
  input: string;
  fileName: string;
  name: string;
  extend?: boolean;
};

const BASE_BUNDLES: Record<string, BundleConfig> = {
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

function discoverIntegrationBundles(): Record<string, BundleConfig> {
  const integrationsDir = path.resolve(__dirname, 'src/integrations');
  if (!fs.existsSync(integrationsDir)) {
    return {};
  }
  const entries = fs.readdirSync(integrationsDir, { withFileTypes: true });
  return entries
    .filter((entry) => entry.isFile() && entry.name.endsWith('.ts'))
    .reduce<Record<string, BundleConfig>>((acc, entry) => {
      const slug = entry.name.replace(/\.ts$/i, '');
      const key = `integration:${slug}`;
      acc[key] = {
        input: path.resolve(integrationsDir, entry.name),
        fileName: `tsjs-${slug}.js`,
        name: `tsjs_${slug.replace(/[^a-zA-Z0-9]/g, '_')}`,
      };
      return acc;
    }, {});
}

const INTEGRATION_BUNDLES = discoverIntegrationBundles();
const BUNDLES: Record<string, BundleConfig> = { ...BASE_BUNDLES, ...INTEGRATION_BUNDLES };

function resolveBundleKey(mode: string | undefined): string {
  const fromEnv = process.env.TSJS_BUNDLE;
  if (fromEnv && BUNDLES[fromEnv]) return fromEnv;

  const normalized = mode?.toLowerCase();
  if (normalized && BUNDLES[normalized]) return normalized;

  if (normalized) {
    const integrationKey = `integration:${normalized}`;
    if (BUNDLES[integrationKey]) {
      return integrationKey;
    }
  }

  return 'core';
}

export default defineConfig(({ mode }) => {
  const bundleKey = resolveBundleKey(mode);
  const bundle = BUNDLES[bundleKey];
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
