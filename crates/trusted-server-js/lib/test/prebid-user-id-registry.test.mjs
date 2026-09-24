// @vitest-environment node

import path from 'node:path';
import { fileURLToPath } from 'node:url';

import { JSDOM } from 'jsdom';
import { build } from 'vite';
import { describe, expect, it } from 'vitest';

import registry from '../src/integrations/prebid/user_id_modules.json';

const libDir = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const prebidDir = path.join(libDir, 'node_modules/prebid.js/dist/src');

describe('Prebid User ID registry matches installed submodules', () => {
  it.each(registry.modules)(
    '$moduleName registers exactly its configured name and aliases',
    async (entry) => {
      const virtualEntry = 'virtual:prebid-user-id-registration';
      const result = await build({
        configFile: false,
        root: libDir,
        logLevel: 'silent',
        // Match the production bridge around LiveIntent's CommonJS wrapper.
        resolve: {
          alias: {
            'prebid.js/modules/liveIntentIdSystem.js': path.join(
              prebidDir,
              'libraries/liveIntentId/idSystem.js'
            ),
          },
        },
        plugins: [
          {
            name: 'capture-user-id-registration',
            resolveId(id) {
              if (id === virtualEntry) return '\0' + virtualEntry;
            },
            load(id) {
              if (id !== '\0' + virtualEntry) return;
              return `
            import ${JSON.stringify(`prebid.js/modules/${entry.moduleName}.js`)};
            import { hook, module } from ${JSON.stringify(path.join(prebidDir, 'src/hook.js'))};
            window.registeredUserIds = [];
            module('userId', (submodule) => {
              window.registeredUserIds.push([submodule.name, submodule.aliasName].filter(Boolean));
            });
            hook.ready();
          `;
            },
          },
        ],
        build: {
          write: false,
          minify: false,
          rollupOptions: {
            input: virtualEntry,
            output: { format: 'iife', name: 'registryTest', inlineDynamicImports: true },
          },
        },
      });

      const dom = new JSDOM('', { url: 'https://publisher.example/', runScripts: 'outside-only' });
      try {
        dom.window.fetch = () => {
          throw new Error('registration must not fetch');
        };
        dom.window.eval(result.output.find((output) => output.type === 'chunk').code);
        expect(dom.window.registeredUserIds).toHaveLength(1);
        expect(dom.window.registeredUserIds[0].map((name) => name.toLowerCase()).sort()).toEqual(
          entry.configNames.map((name) => name.toLowerCase()).sort()
        );
      } finally {
        dom.window.close();
      }
    },
    30_000
  );
});
